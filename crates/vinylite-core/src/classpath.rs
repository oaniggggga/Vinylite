//! Classpath resolution for hierarchy queries.
//!
//! Sources: directories of `.class` files, jar/zip archives (read with the
//! built-in [`crate::zipmini`] reader — no extraction of the whole archive),
//! and in-memory class bytes (the archive being decompiled doubles as its
//! own classpath, so same-jar supertypes resolve with no flags).
//!
//! Only headers are retained (superclass, interfaces, member names +
//! descriptors + flags); method bodies are dropped after parsing.
//! Results are cached, so each class is read and parsed at most once.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::classfile::{ClassFileParser, ConstantPoolEntry};
use crate::recovery::Recoverable;

/// Header info retained for hierarchy walks.
#[derive(Debug, Clone)]
struct ClassHeader {
    super_name: Option<String>,
    interfaces: Vec<String>,
    methods: Vec<MethodHeader>,
}

/// Internal (`com/foo/Bar`) names throughout.
#[derive(Debug, Clone)]
struct MethodHeader {
    name: String,
    descriptor: String,
    flags: u16,
}

fn cp_utf8(pool: &[Recoverable<ConstantPoolEntry>], index: u16) -> Option<String> {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Utf8(value))) => Some(value.clone()),
        _ => None,
    }
}

fn cp_class_name(pool: &[Recoverable<ConstantPoolEntry>], index: u16) -> Option<String> {
    match pool.get(index as usize) {
        Some(Recoverable::Present(ConstantPoolEntry::Class { name_index })) => {
            cp_utf8(pool, *name_index)
        }
        _ => None,
    }
}

fn parse_header(bytes: &[u8]) -> Option<ClassHeader> {
    let mut parser = ClassFileParser::new(bytes);
    let class = parser.parse().ok()?;
    let super_name = if class.super_class == 0 {
        None
    } else {
        cp_class_name(&class.constant_pool, class.super_class)
    };
    let mut methods = Vec::with_capacity(class.methods.len());
    for method in &class.methods {
        let (Some(name), Some(descriptor)) = (
            cp_utf8(&class.constant_pool, method.name_index),
            cp_utf8(&class.constant_pool, method.descriptor_index),
        ) else {
            continue;
        };
        methods.push(MethodHeader {
            name,
            descriptor,
            flags: method.access_flags,
        });
    }
    Some(ClassHeader {
        super_name,
        interfaces: class.interfaces.clone(),
        methods,
    })
}

pub struct Classpath {
    dirs: Vec<PathBuf>,
    archives: Vec<Vec<u8>>,
    /// In-memory classes keyed by archive path (`com/foo/Bar.class`).
    memory: HashMap<String, Vec<u8>>,
    cache: HashMap<String, Option<ClassHeader>>,
}

impl Classpath {
    pub fn new() -> Self {
        Self {
            dirs: Vec::new(),
            archives: Vec::new(),
            memory: HashMap::new(),
            cache: HashMap::new(),
        }
    }

    pub fn add_dir(&mut self, dir: PathBuf) {
        self.dirs.push(dir);
    }

    /// Archive bytes (jar/zip). Non-archives are harmless: lookups simply
    /// find no central directory and miss.
    pub fn add_archive_bytes(&mut self, bytes: Vec<u8>) {
        if bytes.len() >= 4 && bytes[0..4] == [0x50, 0x4b, 0x03, 0x04] {
            self.archives.push(bytes);
        }
    }

    /// Directly index in-memory class bytes (used for the archive currently
    /// being decompiled). `archive_path` uses `/` separators and ends with
    /// `.class`.
    pub fn add_memory_class(&mut self, archive_path: String, bytes: Vec<u8>) {
        self.memory.insert(archive_path, bytes);
    }

    fn raw_bytes(&self, internal_name: &str) -> Option<Vec<u8>> {
        let archive_path = format!("{internal_name}.class");
        if let Some(bytes) = self.memory.get(&archive_path) {
            return Some(bytes.clone());
        }
        let file_name = format!("{internal_name}.class");
        let relative = Path::new(&file_name);
        for dir in &self.dirs {
            if let Ok(bytes) = std::fs::read(dir.join(relative)) {
                return Some(bytes);
            }
        }
        for archive in &self.archives {
            if let Some(bytes) = crate::zipmini::find_class_bytes(archive, &archive_path) {
                return Some(bytes);
            }
        }
        None
    }

    fn header(&mut self, internal_name: &str) -> Option<ClassHeader> {
        if let Some(cached) = self.cache.get(internal_name) {
            return cached.clone();
        }
        let header = self
            .raw_bytes(internal_name)
            .and_then(|bytes| parse_header(&bytes));
        self.cache.insert(internal_name.to_string(), header.clone());
        header
    }

    /// Direct supertypes (superclass first, then interfaces), best-effort.
    fn direct_supertypes(&mut self, internal_name: &str) -> Vec<String> {
        let Some(header) = self.header(internal_name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        if let Some(super_name) = header.super_name {
            out.push(super_name);
        }
        out.extend(header.interfaces);
        out
    }

    /// True when an instance method provably overrides a supertype method:
    /// same name + descriptor found on any transitive supertype, where the
    /// supertype member is neither static nor private. Constructors and
    /// static methods never override. Cycle-safe via a visited set.
    pub fn is_override(
        &mut self,
        class_internal_name: &str,
        method_name: &str,
        descriptor: &str,
        access_flags: u16,
    ) -> bool {
        if method_name.starts_with('<') || access_flags & 0x0008 != 0 {
            return false;
        }
        let mut visited: HashSet<String> = HashSet::new();
        let mut stack = self.direct_supertypes(class_internal_name);
        while let Some(candidate) = stack.pop() {
            if !visited.insert(candidate.clone()) {
                continue;
            }
            let Some(header) = self.header(&candidate) else {
                continue;
            };
            if header.methods.iter().any(|m| {
                m.name == method_name
                    && m.descriptor == descriptor
                    && m.flags & 0x0008 == 0
                    && m.flags & 0x0002 == 0
            }) {
                return true;
            }
            if let Some(super_name) = header.super_name {
                stack.push(super_name);
            }
            stack.extend(header.interfaces.iter().cloned());
        }
        false
    }
}

impl Default for Classpath {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Minimal classfile: header + N methods, no code. Pool layout:
    /// 1=this-name, 2=Class(1), 3=super-name, 4=Class(3),
    /// then per method: Utf8(name), Utf8(descriptor).
    fn make_class(
        name: &str,
        super_name: &str,
        interfaces: &[&str],
        methods: &[(&str, &str, u16)],
    ) -> Vec<u8> {
        let mut pool: Vec<Vec<u8>> = Vec::new();
        fn utf8(pool: &mut Vec<Vec<u8>>, s: &str) -> u16 {
            let mut entry = vec![0x01];
            entry.extend_from_slice(&(s.len() as u16).to_be_bytes());
            entry.extend_from_slice(s.as_bytes());
            pool.push(entry);
            pool.len() as u16
        }
        fn class_entry(pool: &mut Vec<Vec<u8>>, name_index: u16) -> u16 {
            pool.push(vec![0x07, (name_index >> 8) as u8, name_index as u8]);
            pool.len() as u16
        }
        let this_name = utf8(&mut pool, name);
        let this_class = class_entry(&mut pool, this_name);
        let super_name_idx = utf8(&mut pool, super_name);
        let super_class = class_entry(&mut pool, super_name_idx);
        let mut iface_indices = Vec::new();
        for iface in interfaces {
            let name_idx = utf8(&mut pool, iface);
            iface_indices.push(class_entry(&mut pool, name_idx));
        }
        let mut method_blobs = Vec::new();
        for (mname, mdesc, flags) in methods {
            let name_idx = utf8(&mut pool, mname);
            let desc_idx = utf8(&mut pool, mdesc);
            let mut blob = Vec::new();
            blob.extend_from_slice(&flags.to_be_bytes());
            blob.extend_from_slice(&name_idx.to_be_bytes());
            blob.extend_from_slice(&desc_idx.to_be_bytes());
            blob.extend_from_slice(&0u16.to_be_bytes()); // no attributes
            method_blobs.push(blob);
        }
        let mut out = Vec::new();
        out.extend_from_slice(&0xcafebabeu32.to_be_bytes());
        out.extend_from_slice(&0u16.to_be_bytes()); // minor
        out.extend_from_slice(&52u16.to_be_bytes()); // major (Java 8)
        out.extend_from_slice(&((pool.len() + 1) as u16).to_be_bytes());
        for entry in &pool {
            out.extend_from_slice(entry);
        }
        out.extend_from_slice(&0x0021u16.to_be_bytes()); // public super
        out.extend_from_slice(&this_class.to_be_bytes());
        out.extend_from_slice(&super_class.to_be_bytes());
        out.extend_from_slice(&(iface_indices.len() as u16).to_be_bytes());
        for index in iface_indices {
            out.extend_from_slice(&index.to_be_bytes());
        }
        out.extend_from_slice(&0u16.to_be_bytes()); // no fields
        out.extend_from_slice(&(method_blobs.len() as u16).to_be_bytes());
        for blob in &method_blobs {
            out.extend_from_slice(blob);
        }
        out.extend_from_slice(&0u16.to_be_bytes()); // no class attributes
        out
    }

    fn hierarchy() -> Classpath {
        let mut cp = Classpath::new();
        cp.add_memory_class(
            "p/Child.class".to_string(),
            make_class("p/Child", "p/Parent", &[], &[]),
        );
        cp.add_memory_class(
            "p/Parent.class".to_string(),
            make_class(
                "p/Parent",
                "java/lang/Object",
                &["p/Greeter"],
                &[
                    ("direct", "()V", 0x0001),
                    ("helper", "()V", 0x0002), // private: not overridable
                    ("util", "()V", 0x0008),   // static: hides, not overrides
                ],
            ),
        );
        cp.add_memory_class(
            "p/Greeter.class".to_string(),
            make_class(
                "p/Greeter",
                "java/lang/Object",
                &[],
                &[("greet", "(Ljava/lang/Object;)V", 0x0401)],
            ),
        );
        cp
    }

    #[test]
    fn override_via_superclass_and_interface() {
        let mut cp = hierarchy();
        // Direct superclass member.
        assert!(cp.is_override("p/Child", "direct", "()V", 0x0001));
        // Interface member reached transitively via the superclass.
        assert!(cp.is_override("p/Child", "greet", "(Ljava/lang/Object;)V", 0x0001));
        // Private supertype member is not overridable.
        assert!(!cp.is_override("p/Child", "helper", "()V", 0x0001));
        // Static supertype member hides, never overrides.
        assert!(!cp.is_override("p/Child", "util", "()V", 0x0001));
        // Unknown member: no match anywhere.
        assert!(!cp.is_override("p/Child", "missing", "()V", 0x0001));
    }

    #[test]
    fn override_rejects_static_and_constructors() {
        let mut cp = hierarchy();
        assert!(!cp.is_override("p/Child", "greet", "(Ljava/lang/Object;)V", 0x0008));
        assert!(!cp.is_override("p/Child", "<init>", "()V", 0x0001));
    }

    #[test]
    fn missing_hierarchy_is_not_an_override() {
        let mut cp = Classpath::new();
        assert!(!cp.is_override("p/Child", "greet", "(Ljava/lang/Object;)V", 0x0001));
    }

    #[test]
    fn cyclic_hierarchy_terminates() {
        let mut cp = Classpath::new();
        cp.add_memory_class("c/A.class".to_string(), make_class("c/A", "c/B", &[], &[]));
        cp.add_memory_class("c/B.class".to_string(), make_class("c/B", "c/A", &[], &[]));
        assert!(!cp.is_override("c/A", "foo", "()V", 0x0001));
    }
}

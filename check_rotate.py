import zipfile
z = zipfile.ZipFile(r'C:\Users\mrmal\AppData\Local\Temp\opencode\bench\gson.jar')
import struct, io

data = z.read('com/google/gson/internal/LinkedTreeMap.class')
f = io.BytesIO(data)
f.read(4)
minor = struct.unpack('>H', f.read(2))[0]
major = struct.unpack('>H', f.read(2))[0]
cp_count = struct.unpack('>H', f.read(2))[0]
cp = [None]
for i in range(1, cp_count):
    tag = f.read(1)[0]
    if tag == 1:
        length = struct.unpack('>H', f.read(2))[0]
        val = f.read(length).decode('utf-8')
        cp.append(('Utf8', val))
    elif tag in (3,4):
        f.read(4)
    elif tag in (5,6):
        f.read(8)
    elif tag in (7,8):
        f.read(2)
    elif tag in (9,10,11,12):
        f.read(4)
    elif tag == 15:
        f.read(3)
    elif tag == 16:
        f.read(2)
    elif tag in (17,18):
        f.read(4)
    else:
        cp.append((tag, 'unknown'))

access = struct.unpack('>H', f.read(2))[0]
this_class = struct.unpack('>H', f.read(2))[0]
super_class = struct.unpack('>H', f.read(2))[0]
iface_count = struct.unpack('>H', f.read(2))[0]
f.read(iface_count * 2)
field_count = struct.unpack('>H', f.read(2))[0]
for _ in range(field_count):
    f.read(6)
    attr_count = struct.unpack('>H', f.read(2))[0]
    for _ in range(attr_count):
        f.read(2)
        attr_len = struct.unpack('>I', f.read(4))[0]
        f.read(attr_len)

method_count = struct.unpack('>H', f.read(2))[0]
print(f'method_count: {method_count}')
for m in range(method_count):
    access_flags = struct.unpack('>H', f.read(2))[0]
    name_idx = struct.unpack('>H', f.read(2))[0]
    desc_idx = struct.unpack('>H', f.read(2))[0]
    method_name = cp[name_idx][1] if name_idx < len(cp) and cp[name_idx] and cp[name_idx][0] == 'Utf8' else '???'
    attr_count = struct.unpack('>H', f.read(2))[0]
    if 'rotate' in method_name:
        print(f'  {method_name}: attr_count={attr_count}')
        for _ in range(attr_count):
            name_idx2 = struct.unpack('>H', f.read(2))[0]
            attr_len = struct.unpack('>I', f.read(4))[0]
            attr_data = f.read(attr_len)
            attr_name = cp[name_idx2][1] if name_idx2 < len(cp) and cp[name_idx2] and cp[name_idx2][0] == 'Utf8' else '???'
            if attr_name == 'Code':
                max_stack = struct.unpack('>H', attr_data[:2])[0]
                max_locals = struct.unpack('>H', attr_data[2:4])[0]
                code_len = struct.unpack('>I', attr_data[4:8])[0]
                code_bytes = attr_data[8:8+code_len]
                print(f'    Code: max_stack={max_stack}, max_locals={max_locals}, code_len={code_len}')
                for i, b in enumerate(code_bytes):
                    if b == 0xb5 or b == 0xb4:
                        opcode_name = 'putfield' if b == 0xb5 else 'getfield'
                        print(f'    offset={i}: {opcode_name}')
    else:
        for _ in range(attr_count):
            f.read(2)
            attr_len = struct.unpack('>I', f.read(4))[0]
            f.read(attr_len)
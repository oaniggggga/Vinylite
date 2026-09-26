import zipfile
z = zipfile.ZipFile(r'C:\Users\mrmal\AppData\Local\Temp\opencode\bench\commons-lang3.jar')
import struct, io

data = z.read('org/apache/commons/lang3/ArrayUtils.class')
f = io.BytesIO(data)
f.read(4)
minor = struct.unpack('>H', f.read(2))[0]
major = struct.unpack('>H', f.read(2))[0]
cp_count = struct.unpack('>H', f.read(2))[0]
print(f'cp_count: {cp_count}')
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
        pass

access = struct.unpack('>H', f.read(2))[0]
this_class = struct.unpack('>H', f.read(2))[0]
super_class = struct.unpack('>H', f.read(2))[0]
iface_count = struct.unpack('>H', f.read(2))[0]
f.read(iface_count * 2)
field_count = struct.unpack('>H', f.read(2))[0]
print(f'field_count: {field_count}')
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
    attr_count = struct.unpack('>H', f.read(2))[0]
    print(f'  method {m}: name_idx={name_idx}, desc_idx={desc_idx}, attr_count={attr_count}')
    for _ in range(attr_count):
        name_idx2 = struct.unpack('>H', f.read(2))[0]
        attr_len = struct.unpack('>I', f.read(4))[0]
        # Just skip
        f.read(attr_len)
print('Done parsing methods')
import zipfile
z = zipfile.ZipFile(r'C:\Users\mrmal\AppData\Local\Temp\opencode\bench\commons-lang3.jar')
import struct, io

data = z.read('org/apache/commons/lang3/ArrayUtils.class')
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
        cp.append((tag, f.read(4)))
    elif tag in (5,6):
        cp.append((tag, f.read(8)))
    elif tag in (7,8):
        cp.append((tag, f.read(2)))
    elif tag in (9,10,11,12):
        cp.append((tag, struct.unpack('>HH', f.read(4))))
    elif tag == 15:
        cp.append((tag, struct.unpack('>BH', f.read(3))))
    elif tag == 16:
        cp.append((tag, f.read(2)))
    elif tag in (17,18):
        cp.append((tag, struct.unpack('>HH', f.read(4))))
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
    method_desc = cp[desc_idx][1] if desc_idx < len(cp) and cp[desc_idx] and cp[desc_idx][0] == 'Utf8' else '???'
    attr_count = struct.unpack('>H', f.read(2))[0]
    has_lvt = False
    for _ in range(attr_count):
        name_idx2 = struct.unpack('>H', f.read(2))[0]
        attr_len = struct.unpack('>I', f.read(4))[0]
        attr_data = f.read(attr_len)
        attr_name = cp[name_idx2][1] if name_idx2 < len(cp) and cp[name_idx2] and cp[name_idx2][0] == 'Utf8' else '???'
        if attr_name == 'LocalVariableTable':
            has_lvt = True
            lvt_count = struct.unpack('>H', attr_data[:2])[0]
            print(f'  {method_name} {method_desc} -> LVT count: {lvt_count}')
            pos = 2
            for i in range(lvt_count):
                if pos + 10 <= len(attr_data):
                    start_pc = struct.unpack('>H', attr_data[pos:pos+2])[0]
                    length = struct.unpack('>H', attr_data[pos+2:pos+4])[0]
                    name_idx3 = struct.unpack('>H', attr_data[pos+4:pos+6])[0]
                    desc_idx3 = struct.unpack('>H', attr_data[pos+6:pos+8])[0]
                    index = struct.unpack('>H', attr_data[pos+8:pos+10])[0]
                    name = cp[name_idx3][1] if name_idx3 < len(cp) and cp[name_idx3] and cp[name_idx3][0] == 'Utf8' else '???'
                    print(f'    LVT[{i}]: start_pc={start_pc}, len={length}, name={name}, index={index}')
                    pos += 10
    if not has_lvt and 'sort' in method_name.lower():
        print(f'  {method_name} {method_desc} -> NO LVT')
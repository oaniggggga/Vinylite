import zipfile, struct, io
z = zipfile.ZipFile(r'C:\Users\mrmal\AppData\Local\Temp\opencode\bench\gson.jar')
data = z.read('com/google/gson/TypeAdapter.class')
f = io.BytesIO(data)
f.read(4)
minor = struct.unpack('>H', f.read(2))[0]
major = struct.unpack('>H', f.read(2))[0]
cp_count = struct.unpack('>H', f.read(2))[0]
print('cp_count:', cp_count)

cp = [None]
for i in range(1, cp_count):
    tag = f.read(1)[0]
    if tag == 1:
        length = struct.unpack('>H', f.read(2))[0]
        val = f.read(length).decode('utf-8')
        cp.append(('Utf8', val))
    elif tag in (3, 4):
        cp.append((tag, f.read(4)))
    elif tag in (5, 6):
        cp.append((tag, f.read(8)))
    elif tag in (7, 8):
        cp.append((tag, f.read(2)))
    elif tag in (9, 10, 11):
        cp.append((tag, struct.unpack('>HH', f.read(4))))
    elif tag == 12:
        cp.append((tag, struct.unpack('>HH', f.read(4))))
    elif tag == 15:
        cp.append((tag, struct.unpack('>BH', f.read(3))))
    elif tag == 16:
        cp.append((tag, f.read(2)))
    elif tag in (17, 18):
        cp.append((tag, struct.unpack('>HH', f.read(4))))
    else:
        cp.append((tag, 'unknown'))
print('CP entries:', len(cp))
for i, entry in enumerate(cp):
    if entry and entry[0] == 'Utf8' and 'Signature' in entry[1]:
        print(f'  [{i}] {entry}')

access = struct.unpack('>H', f.read(2))[0]
this_class = struct.unpack('>H', f.read(2))[0]
super_class = struct.unpack('>H', f.read(2))[0]
print(f'access={access:04x}, this={this_class}, super={super_class}')
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
for _ in range(method_count):
    f.read(6)
    attr_count = struct.unpack('>H', f.read(2))[0]
    for _ in range(attr_count):
        f.read(2)
        attr_len = struct.unpack('>I', f.read(4))[0]
        f.read(attr_len)
attr_count = struct.unpack('>H', f.read(2))[0]
print(f'class attr_count={attr_count}')
for _ in range(attr_count):
    name_idx = struct.unpack('>H', f.read(2))[0]
    attr_len = struct.unpack('>I', f.read(4))[0]
    attr_data = f.read(attr_len)
    name = cp[name_idx][1] if name_idx < len(cp) else '???'
    if 'Signature' in name:
        sig_idx = struct.unpack('>H', attr_data[:2])[0]
        sig_val = cp[sig_idx][1] if sig_idx < len(cp) else '???'
        print(f'  Signature attr: name_idx={name_idx}, len={attr_len}, sig_idx={sig_idx}, sig={sig_val}')
    else:
        print(f'  attr: {name}, len={attr_len}')
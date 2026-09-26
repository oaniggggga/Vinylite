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
        print(f'unknown tag {tag} at cp index {i}')
        break

print(f'CP done, pos={f.tell()}')
access = struct.unpack('>H', f.read(2))[0]
print(f'access: {access:04x}')
this_class = struct.unpack('>H', f.read(2))[0]
super_class = struct.unpack('>H', f.read(2))[0]
iface_count = struct.unpack('>H', f.read(2))[0]
print(f'this={this_class}, super={super_class}, iface_count={iface_count}')
f.read(iface_count * 2)
field_count = struct.unpack('>H', f.read(2))[0]
print(f'field_count: {field_count}')
print(f'pos after field_count: {f.tell()}')
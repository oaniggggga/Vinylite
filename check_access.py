import zipfile
z = zipfile.ZipFile(r'C:\Users\mrmal\AppData\Local\Temp\opencode\bench\commons-lang3.jar')
import struct, io
data = z.read('org/apache/commons/lang3/ArrayUtils.class')
f = io.BytesIO(data)
f.read(4)
minor = struct.unpack('>H', f.read(2))[0]
major = struct.unpack('>H', f.read(2))[0]
cp_count = struct.unpack('>H', f.read(2))[0]
for i in range(1, cp_count):
    tag = f.read(1)[0]
    if tag == 1:
        length = struct.unpack('>H', f.read(2))[0]
        f.read(length)
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
access = struct.unpack('>H', f.read(2))[0]
with open('access.txt', 'w') as out:
    out.write(f'access_flags: {access:04x}\n')
    out.write(f'is_interface: {bool(access & 0x0200)}\n')
    out.write(f'is_annotation: {bool(access & 0x2000)}\n')
    out.write(f'is_enum: {bool(access & 0x4000)}\n')
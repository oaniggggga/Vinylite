import zipfile
z = zipfile.ZipFile(r'C:\Users\mrmal\AppData\Local\Temp\opencode\bench\commons-lang3.jar')
import struct, io

data = z.read('org/apache/commons/lang3/ArrayUtils.class')
# Print raw bytes around the access flags area
# magic(4) + minor(2) + major(2) + cp_count(2) + cp entries...
# Let's find the access flags by scanning
for i in range(100, 300):
    if i + 2 <= len(data):
        val = struct.unpack('>H', data[i:i+2])[0]
        if val == 0x2100 or val == 0x0100 or val == 0x0021:
            print(f'Offset {i}: {val:04x}')
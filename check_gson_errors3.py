import os, re, subprocess
JAVAC = r"C:\Program Files\Java\jdk-25.0.2\bin\javac.exe"
SRC = r"C:\Users\mrmal\AppData\Local\Temp\opencode\bench\gson-vinylite"
files = []
for dp, _, fns in os.walk(SRC):
    for fn in fns:
        if fn.endswith(".java"):
            files.append(os.path.join(dp, fn).replace("\\", "/"))
argfile = "gson-err3.txt"
open(argfile, "w", encoding="utf-8").write("\n".join(files))
dest = "gson-err3-out"
os.makedirs(dest, exist_ok=True)
r = subprocess.run([JAVAC, "-nowarn", "-Xmaxerrs", "100000", "-d", dest, "-sourcepath", SRC, "@" + argfile], capture_output=True, text=True, timeout=600)
print("STDOUT:", r.stdout[:500])
print("STDERR:", r.stderr[:5000])
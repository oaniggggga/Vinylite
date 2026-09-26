import os, re, subprocess
JAVAC = r"C:\Program Files\Java\jdk-25.0.2\bin\javac.exe"
OUT = r"C:\Users\mrmal\AppData\Local\Temp\opencode\recomp"
SRC = r"C:\Users\mrmal\Desktop\Agent\betterpilot\gson_decompiled"
files = []
for dp, _, fns in os.walk(SRC):
    for fn in fns:
        if fn.endswith(".java"):
            files.append(os.path.join(dp, fn).replace("\\", "/"))
argfile = os.path.join(OUT, "gson-test.txt")
open(argfile, "w", encoding="utf-8").write("\n".join(files))
dest = os.path.join(OUT, "gson-test")
os.makedirs(dest, exist_ok=True)
r = subprocess.run([JAVAC, "-nowarn", "-Xmaxerrs", "100000", "-d", dest, "-sourcepath", SRC, "@" + argfile], capture_output=True, text=True, timeout=600)
with open(os.path.join(OUT, "compile_out.txt"), "w") as f:
    f.write("STDOUT:\n")
    f.write(r.stdout or "")
    f.write("\nSTDERR:\n")
    f.write(r.stderr or "")
print("Done, rc:", r.returncode)
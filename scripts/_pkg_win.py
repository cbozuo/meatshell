#!/usr/bin/env python3
# 把 target/x86_64-pc-windows-gnu/release/meatshell.exe 打成一个可分发 zip 验证包。
# 同时用 objdump 列出动态依赖，把非系统 DLL (libgcc_s/libstdc++/libwinpthread)
# 从 llvm-mingw/bin 一并拷进包，避免目标机器缺运行时。
import os, shutil, subprocess, zipfile, sys

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
# 本仓库无 MSVC，交叉编译产物在 gnu target 下（不是 target/release/）
EXE = os.path.join(REPO, "target", "x86_64-pc-windows-gnu", "release", "meatshell.exe")
LLVM_MINGW_BIN = r"C:\llvm-mingw\bin"
# (#pkg-toolchain-detect 2026-09-04) 原常量保留为首个候选：本机 C:\llvm-mingw
# 已不存在，真正完成 x86_64-pc-windows-gnu 链接的是 MSYS2 的 mingw64（cargo 从
# PATH 里找到它）。硬编码路径一旦缺失，第 3 步会把所有运行时 DLL 都判为"未找到"，
# 打出来的包在目标机器上缺 libgcc_s/libstdc++/libwinpthread 而启动失败。
# 因此逐个探测候选项，取第一个真实存在的目录。
for _cand in [r"C:\llvm-mingw\bin",
              r"C:\Users\Administrator\msys64\mingw64\bin",
              r"C:\msys64\mingw64\bin"]:
    if os.path.isdir(_cand):
        LLVM_MINGW_BIN = _cand
        break
# objdump 的名字随发行版而异：llvm-mingw 用的是带 target 前缀的真实二进制（其裸
# `objdump` 是无扩展 shell 脚本，Windows 无法直接 exec），MSYS2 则是裸 objdump.exe。
# 逐个尝试，全缺失时提前退出，避免 subprocess 抛难懂的 FileNotFoundError。
OBJDUMP = None
for _name in ["x86_64-w64-mingw32-objdump.exe", "llvm-objdump.exe", "objdump.exe"]:
    _p = os.path.join(LLVM_MINGW_BIN, _name)
    if os.path.isfile(_p):
        OBJDUMP = _p
        break
if OBJDUMP is None:
    sys.exit(f"ERROR: 在 {LLVM_MINGW_BIN} 下找不到 objdump，无法分析动态依赖")
STAGE_NAME = "meatshell-win-verify"
STAGE = os.path.join(REPO, STAGE_NAME)
ZIP_PATH = os.path.join(REPO, STAGE_NAME + ".zip")

if not os.path.isfile(EXE):
    sys.exit(f"ERROR: {EXE} 不存在，先 cargo build --release --target x86_64-pc-windows-gnu")

shutil.rmtree(STAGE, ignore_errors=True)
os.makedirs(STAGE, exist_ok=True)

# 1) 复制主程序
shutil.copy(EXE, os.path.join(STAGE, "meatshell.exe"))

# 2) 复制文档
for f in ["README.md", "README.en.md", "CHANGELOG.md", "THIRD_PARTY_NOTICES.md"]:
    src = os.path.join(REPO, f)
    if os.path.isfile(src):
        shutil.copy(src, os.path.join(STAGE, f))

# 3) 收集动态依赖
out = subprocess.run(
    [OBJDUMP, "-p", EXE], capture_output=True, text=True, check=True
).stdout
dlls = []
for line in out.splitlines():
    line = line.strip()
    if line.startswith("DLL Name:"):
        dlls.append(line.split("DLL Name:")[1].strip())

# 已知 Windows 系统 DLL（通常位于 System32，无需捆绑）
SYSTEM_DLLS = {
    "KERNEL32.DLL","USER32.DLL","GDI32.DLL","ADVAPI32.DLL","SHELL32.DLL",
    "OLE32.DLL","OLEAUT32.DLL","WINMM.DLL","SETUPAPI.DLL","VERSION.DLL",
    "SHLWAPI.DLL","COMCTL32.DLL","COMDLG32.DLL","CRYPT32.DLL","WS2_32.DLL",
    "MSVCRT.DLL","IMM32.DLL","NETAPI32.DLL","NTDLL.DLL","RPCRT4.DLL",
    "USERENV.DLL","WINTRUST.DLL","DBGHELP.DLL","PSAPI.DLL","SECUR32.DLL",
    "BCRYPT.DLL","BCRYPTPRIMITIVES.DLL","DWMAPI.DLL","IPHLPAPI.DLL","PDH.DLL",
    "POWRPROF.DLL","UXTHEME.DLL","OLEAUT32.DLL","UIAUTOMATIONCORE.DLL",
    "DWRITE.DLL","OPENGL32.DLL","SHLWAPI.DLL",
}

def is_system(d):
    if d.upper() in SYSTEM_DLLS:
        return True
    if d.upper().startswith("API-MS-WIN") or d.upper().startswith("EXT-MS-WIN"):
        return True
    return False

bundled = []
for d in dlls:
    if is_system(d):
        continue
    src = os.path.join(LLVM_MINGW_BIN, d)
    if os.path.isfile(src):
        shutil.copy(src, os.path.join(STAGE, d))
        bundled.append(d)
    else:
        print(f"  [warn] 非系统 DLL 但未在 {LLVM_MINGW_BIN} 找到: {d}")

# 4) 打包
if os.path.isfile(ZIP_PATH):
    os.remove(ZIP_PATH)
with zipfile.ZipFile(ZIP_PATH, "w", zipfile.ZIP_DEFLATED) as z:
    for root, _, files in os.walk(STAGE):
        for fn in files:
            fp = os.path.join(root, fn)
            z.write(fp, os.path.relpath(fp, REPO))

print("已打包:", ZIP_PATH)
print("捆绑的非系统 DLL:", bundled if bundled else "无 (纯系统依赖，开箱即用)")
print("总文件数:", len(os.listdir(STAGE)))

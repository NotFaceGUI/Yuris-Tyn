# -*- coding: utf-8 -*-
# 反编译 CGACT 处理器(运行期填充表,静态零引用 ⇒ 未被识别为函数;先建函数再反编译)。
# 用法: analyzeHeadless <projdir> ghidra-proj -process <game-b> -noanalysis \
#         -scriptPath <本目录> -postScript decompile_cgact.py 42c438
# 输出: <side-repo>/workbench/tmp/ghidra_all/<prog>/extra/
# @category Yuris
import os
from ghidra.app.decompiler import DecompInterface
from ghidra.util.task import ConsoleTaskMonitor
from ghidra.app.cmd.disassemble import DisassembleCommand
from ghidra.app.cmd.function import CreateFunctionCmd

prog = currentProgram
fm = prog.getFunctionManager()
monitor = ConsoleTaskMonitor()
outdir = r"<side-repo>\workbench\tmp\ghidra_all"
if not os.path.exists(outdir):
    os.makedirs(outdir)

args = getScriptArgs()
start = int(args[0], 16)

af = prog.getAddressFactory().getDefaultAddressSpace()
addr_start = af.getAddress(start)

funcs = fm.getFunctionsContaining(addr_start)
if funcs.isEmpty():
    cmd = DisassembleCommand(addr_start, None, True)
    cmd.applyTo(prog, monitor)
    fcmd = CreateFunctionCmd("FUN_%08x" % start, addr_start, None,
                             ghidra.program.model.symbol.SourceType.USER_DEFINED)
    fcmd.applyTo(prog, monitor)

f = fm.getFunctionContaining(addr_start)
print("function: %s" % f)
if f is None:
    print("NO FUNCTION CREATED")
else:
    decomp = DecompInterface()
    decomp.openProgram(prog)
    res = decomp.decompileFunction(f, 600, monitor)
    if res.decompileCompleted():
        code = res.getDecompiledFunction().getC()
        fname = "%08x_%s.c" % (f.getEntryPoint().getOffset(),
                               f.getName().replace(' ', '_'))
        fh = open(os.path.join(outdir, fname), 'wb')
        fh.write(("// %s @ %s size=%d\n" % (f.getName(), f.getEntryPoint(),
                                            f.getBody().getNumAddresses())).encode('utf-8'))
        fh.write(code.encode('utf-8') if isinstance(code, unicode) else code)
        fh.close()
        print("WROTE %s" % os.path.join(outdir, fname))
    else:
        print("DECOMPILE FAILED: %s" % res.getErrorMessage())

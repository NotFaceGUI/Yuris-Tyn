// 反编译运行期填充表里的 CGACT 处理器(静态零引用 ⇒ 未被识别为函数;先建函数再反编译)。
// 用法: analyzeHeadless <projdir> <game-b> -process <game-b>_v494.exe -noanalysis \
//         -scriptPath <本目录> -postScript DecompileCgact.java 42c438
// 输出: <side-repo>/workbench/tmp/ghidra_all/
import java.io.File;
import java.io.FileWriter;
import java.io.PrintWriter;

import ghidra.app.script.GhidraScript;
import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.cmd.disassemble.DisassembleCommand;
import ghidra.app.cmd.function.CreateFunctionCmd;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.symbol.SourceType;
import ghidra.util.task.ConsoleTaskMonitor;

public class DecompileCgact extends GhidraScript {
    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        long start = Long.parseLong(args[0], 16);
        File outdir = new File("<side-repo>/workbench/tmp/ghidra_all");
        outdir.mkdirs();

        Address addr = currentProgram.getAddressFactory().getDefaultAddressSpace()
                .getAddress(start);
        Function f = getFunctionContaining(addr);
        if (f == null) {
            DisassembleCommand dc = new DisassembleCommand(addr, null, true);
            dc.applyTo(currentProgram, monitor);
            CreateFunctionCmd cc = new CreateFunctionCmd(
                    "FUN_%08x".formatted(start), addr, null, SourceType.USER_DEFINED);
            cc.applyTo(currentProgram, monitor);
            f = getFunctionContaining(addr);
        }
        println("function: " + f);
        if (f == null) {
            println("NO FUNCTION CREATED");
            return;
        }
        DecompInterface decomp = new DecompInterface();
        decomp.openProgram(currentProgram);
        DecompileResults res = decomp.decompileFunction(f, 600, monitor);
        if (res.decompileCompleted()) {
            String code = res.getDecompiledFunction().getC();
            File out = new File(outdir, "%08x_%s.c".formatted(
                    f.getEntryPoint().getOffset(), f.getName().replace(' ', '_')));
            try (PrintWriter pw = new PrintWriter(new FileWriter(out))) {
                pw.printf("// %s @ %s size=%d%n", f.getName(), f.getEntryPoint(),
                        f.getBody().getNumAddresses());
                pw.print(code);
            }
            println("WROTE " + out);
        } else {
            println("DECOMPILE FAILED: " + res.getErrorMessage());
        }
    }
}

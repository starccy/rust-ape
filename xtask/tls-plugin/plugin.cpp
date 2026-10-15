// LLVM pass plugin rustc loads with -Z llvm-plugins. Every
// llvm.threadlocal.address becomes inline asm that finds the thread
// pointer through the tib, the way the getters in the fork's
// libc/sysv/tlsasm.S do, and adds the variable's offset to it. The
// variables stay in .tdata/.tbss, laid out by the linker.

#include "llvm/IR/IRBuilder.h"
#include "llvm/IR/InlineAsm.h"
#include "llvm/IR/IntrinsicInst.h"
#include "llvm/IR/Module.h"
#include "llvm/Passes/PassBuilder.h"
#include "llvm/Plugins/PassPlugin.h"
#include "llvm/Support/ErrorHandling.h"
#include "llvm/TargetParser/Triple.h"

using namespace llvm;

namespace {

// thread pointer in rax; a thread with no tib yet gets one from
// __get_tls_rax, which keeps every other register
const char kX86Base[] = "movl __hostos(%rip), %eax\n\t"
                        "testl $$12, %eax\n\t"
                        "jnz 1f\n\t"
                        "movq %fs:0x38, %rax\n\t"
                        "jmp 2f\n"
                        "1:\n\t"
                        "testl $$4, %eax\n\t"
                        "jnz 3f\n\t"
                        "movq %gs:0x30, %rax\n\t"
                        "jmp 4f\n"
                        "3:\n\t"
                        "movl __tls_index(%rip), %eax\n\t"
                        "movq %gs:0x1480(,%rax,8), %rax\n"
                        "2:\n\t"
                        "testq %rax, %rax\n\t"
                        "jnz 4f\n\t"
                        "call __get_tls_rax\n"
                        "4:";

const char kX86Off[] = "leaq ${2:c}@tpoff($1), $0";

// thread pointer in x0, which is the address just past the tib
const char kA64Base[] = "adrp x0, __hostos\n\t"
                        "ldr w0, [x0, :lo12:__hostos]\n\t"
                        "tbnz w0, #3, 1f\n\t"
                        "mrs x0, tpidr_el0\n\t"
                        "ldr x0, [x0, #8]\n\t"
                        "cbnz x0, 2f\n"
                        "1:\n\t"
                        "bl __get_tls_x0\n\t"
                        "b 3f\n"
                        "2:\n\t"
                        "add x0, x0, #1024\n"
                        "3:";

const char kA64Off[] = "add $0, $1, :tprel_hi12:${2:c}\n\t"
                       "add $0, $0, :tprel_lo12_nc:${2:c}";

struct Target {
  const char *base, *baseCons, *off;
};

bool target(const Module &M, Target &T) {
  Triple TT(M.getTargetTriple());
  if (TT.getEnvironment() != Triple::Musl)
    return false;
  if (TT.getArch() == Triple::x86_64) {
    T = {kX86Base, "={rax},~{cc},~{dirflag},~{fpsr},~{flags}", kX86Off};
    return true;
  }
  if (TT.getArch() == Triple::aarch64) {
    T = {kA64Base, "={x0},~{x30},~{cc}", kA64Off};
    return true;
  }
  return false;
}

bool isOffsetAsm(const User *U, const Target &T) {
  auto *CI = dyn_cast<CallInst>(U);
  if (!CI)
    return false;
  auto *IA = dyn_cast<InlineAsm>(CI->getCalledOperand());
  return IA && IA->getAsmString() == T.off;
}

// a use in @llvm.used or @llvm.compiler.used
bool isUsedList(const User *U) {
  if (auto *GV = dyn_cast<GlobalVariable>(U))
    return GV->getName() == "llvm.used" || GV->getName() == "llvm.compiler.used";
  if (!isa<Constant>(U))
    return false;
  for (const User *V : U->users())
    if (!isUsedList(V))
      return false;
  return true;
}

void rewrite(Module &M, const Target &T) {
  SmallVector<IntrinsicInst *, 16> calls;
  for (Function &F : M)
    if (F.getIntrinsicID() == Intrinsic::threadlocal_address)
      for (User *U : F.users())
        if (auto *II = dyn_cast<IntrinsicInst>(U))
          calls.push_back(II);
  for (IntrinsicInst *II : calls) {
    Value *G = II->getArgOperand(0);
    Type *P = II->getType();
    IRBuilder<> B(II);
    auto *BaseIA = InlineAsm::get(FunctionType::get(P, false), T.base,
                                  T.baseCons, /*hasSideEffects=*/false);
    CallInst *TP = B.CreateCall(BaseIA);
    TP->addFnAttr(Attribute::NoUnwind);
    TP->setDoesNotAccessMemory();
    auto *OffIA =
        InlineAsm::get(FunctionType::get(P, {P, G->getType()}, false), T.off,
                       "=r,r,s", /*hasSideEffects=*/false);
    CallInst *CI = B.CreateCall(OffIA, {TP, G});
    CI->addFnAttr(Attribute::NoUnwind);
    CI->setDoesNotAccessMemory();
    II->replaceAllUsesWith(CI);
    II->eraseFromParent();
  }
  for (GlobalVariable &GV : M.globals()) {
    if (!GV.isThreadLocal())
      continue;
    for (const User *U : GV.users())
      if (!isOffsetAsm(U, T) && !isUsedList(U))
        reportFatalUsageError(
            Twine("rust-ape: thread-local ") + GV.getName() +
            " is used other than through llvm.threadlocal.address, so its "
            "access would go to the host's thread register");
  }
}

struct TlsRewrite : PassInfoMixin<TlsRewrite> {
  PreservedAnalyses run(Module &M, ModuleAnalysisManager &) {
    Target T;
    if (!target(M, T))
      return PreservedAnalyses::all();
    rewrite(M, T);
    return PreservedAnalyses::none();
  }
  static bool isRequired() { return true; }
};

} // namespace

extern "C" LLVM_ATTRIBUTE_WEAK PassPluginLibraryInfo llvmGetPassPluginInfo() {
  return {LLVM_PLUGIN_API_VERSION, "rust-ape-tls", "1", [](PassBuilder &PB) {
            PB.registerPipelineStartEPCallback(
                [](ModulePassManager &MPM, OptimizationLevel) {
                  MPM.addPass(TlsRewrite());
                });
            PB.registerOptimizerLastEPCallback([](ModulePassManager &MPM,
                                                  OptimizationLevel,
                                                  ThinOrFullLTOPhase) {
              MPM.addPass(TlsRewrite());
            });
            PB.registerFullLinkTimeOptimizationLastEPCallback(
                [](ModulePassManager &MPM, OptimizationLevel) {
                  MPM.addPass(TlsRewrite());
                });
          }};
}

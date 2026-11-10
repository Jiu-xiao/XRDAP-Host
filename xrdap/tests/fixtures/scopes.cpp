/*
 * 变量作用域的测试夹具，只用来生成 scopes*.elf，不在目标上运行。编译命令见同目录的
 * build.sh：同一份源文件编成普通、LTO、压缩调试段三个 ELF；scopes-nodebug.c 不带 -g。
 */
#include <stdint.h>

volatile uint32_t g_global = 1;
static volatile uint32_t s_file = 2;
namespace motor { volatile float speed = 3; }
namespace { volatile int anon_ns = 4; }
namespace left { volatile int dup = 10; }
namespace right { volatile int dup = 11; }

struct Pid { float kp, ki, out; };
class Ctrl {
public:
  static Pid pid;
  static volatile int count;
};
Pid Ctrl::pid = {1, 2, 3};
volatile int Ctrl::count = 5;

template <int N> struct Holder { static volatile int v; };
template <int N> volatile int Holder<N>::v = N;

// 没有被引用，--gc-sections 丢掉它，DWARF 里留下地址 0
volatile int unused_var = 9;

extern "C" volatile uint32_t lib_nodebug;
const char *volatile g_name;

uint32_t tick() {
  static volatile uint32_t f_static = 0;
  {
    static volatile uint32_t blk_static = 0;
    blk_static = blk_static + 1;
  }
  g_name = __func__;
  return f_static = f_static + 1;
}

extern "C" void Reset_Handler() {
  auto lam = [] {
    static volatile int lam_static = 7;
    return lam_static = lam_static + 1;
  };
  for (;;) {
    tick();
    lam();
    g_global = g_global + s_file + anon_ns + left::dup + right::dup + Holder<3>::v + Ctrl::count +
               (int)Ctrl::pid.kp + (int)motor::speed + lib_nodebug;
  }
}

/* 函数作用域的 static 变量：有固定地址，和文件作用域的 static 一样应该能被找出来。
   单独一个编译单元，编译成 statics.elf，不改变 types.elf 里各变量的地址。 */

typedef unsigned int uint32_t;

void statics_touch(void);

/* volatile 让编译器不能把从没改过的 static 折成常量，必须给它分配存储 */
static volatile uint32_t touched;

void statics_touch(void)
{
  /* 有初值的 static：在 .data */
  static uint32_t func_static = 0x5A5A5A5Au;
  /* 没有初值的 static：在 .bss */
  static uint32_t func_zero;
  func_static++;
  func_zero++;
  touched = func_static + func_zero;
}

/* 入口符号，和 types.ld 里的 ENTRY 对上 */
void Reset_Handler(void)
{
  statics_touch();
  for (;;)
  {
  }
}

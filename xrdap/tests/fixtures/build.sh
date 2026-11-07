#!/bin/sh
# 重新生成测试用的 ELF。需要 Arm GNU Toolchain（arm-none-eabi-gcc 14.2）。
# 同一份 types.c 编两次：types.elf 用 DWARF 4，types-dwarf5.elf 用 DWARF 5，覆盖结构体、数组、
# 指针、浮点、位域、联合、枚举等类型。调试信息里的路径都换成相对路径，ELF 不带编译机器的目录。
set -e
cd "$(dirname "$0")"
CC=${CC:-arm-none-eabi-gcc}
# pwd -W 是 Git Bash 上的 Windows 路径，和编译器看到的一样；别的系统没有这个选项
here=$(pwd -W 2>/dev/null || pwd)
toolchain=$(cd "$(dirname "$(command -v "$CC")")/.." && { pwd -W 2>/dev/null || pwd; })
for v in 4 5; do
  out=types.elf
  [ "$v" = 5 ] && out=types-dwarf5.elf
  "$CC" -mcpu=cortex-m4 -mthumb -mfloat-abi=soft -O0 -g -gdwarf-$v -ffreestanding -nostdlib -nostartfiles \
    -ffile-prefix-map="$here"=. -ffile-prefix-map="$toolchain"=toolchain \
    -Wall -Wextra -Werror -T types.ld -o "$out" types.c
done

#!/bin/sh
# 重新生成测试用的 ELF。需要 Arm GNU Toolchain（arm-none-eabi-gcc 14.2）。
# types.c 编两次：types.elf 用 DWARF 4，types-dwarf5.elf 用 DWARF 5，覆盖结构体、数组、
# 指针、浮点、位域、联合、枚举等类型。statics.c 同样编两次：statics.elf 和 statics-dwarf5.elf，
# 覆盖函数作用域的 static 变量。scopes.cpp 编成三个 ELF：scopes.elf（普通）、scopes-lto.elf
# （-flto）、scopes-gz.elf（压缩调试段），覆盖命名空间、类的静态成员、函数内的 static 变量和被
# --gc-sections 丢掉的变量。调试信息里的路径都换成相对路径，ELF 不带编译机器的目录。
set -e
cd "$(dirname "$0")"
CC=${CC:-arm-none-eabi-gcc}
CXX=${CXX:-arm-none-eabi-g++}
# pwd -W 是 Git Bash 上的 Windows 路径，和编译器看到的一样；别的系统没有这个选项
here=$(pwd -W 2>/dev/null || pwd)
toolchain=$(cd "$(dirname "$(command -v "$CC")")/.." && { pwd -W 2>/dev/null || pwd; })
for v in 4 5; do
  suffix=
  [ "$v" = 5 ] && suffix=-dwarf5
  for src in types statics; do
    "$CC" -mcpu=cortex-m4 -mthumb -mfloat-abi=soft -O0 -g -gdwarf-$v -ffreestanding -nostdlib -nostartfiles \
      -ffile-prefix-map="$here"=. -ffile-prefix-map="$toolchain"=toolchain \
      -Wall -Wextra -Werror -T types.ld -o "$src$suffix.elf" "$src.c"
  done
done
arch="-mcpu=cortex-m4 -mthumb -mfloat-abi=soft"
"$CC" $arch -O0 -ffreestanding -c -o scopes-nodebug.o scopes-nodebug.c
for kind in plain lto gz; do
  out=scopes-$kind.elf
  flags=""
  [ "$kind" = plain ] && out=scopes.elf
  [ "$kind" = lto ] && flags="-O1 -flto"
  [ "$kind" = gz ] && flags="-gz"
  "$CXX" $arch -O0 $flags -g -gdwarf-4 -ffreestanding -fno-exceptions -fno-rtti -nostdlib -nostartfiles \
    -ffunction-sections -fdata-sections -Wl,--gc-sections \
    -ffile-prefix-map="$here"=. -ffile-prefix-map="$toolchain"=toolchain \
    -Wall -Wextra -Werror -T types.ld -o "$out" scopes.cpp scopes-nodebug.o -lgcc
done
rm -f scopes-nodebug.o

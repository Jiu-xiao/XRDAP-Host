#!/bin/sh
# 重新生成测试用的 ELF。需要 Arm GNU Toolchain（arm-none-eabi-gcc 14.2）。
# g431-test.elf 由 test.c 用目标测试板的 link.ld 编译（-O2 -g，DWARF 5），这里只保留源码和 ELF。
# types.elf 用 DWARF 4 编译，覆盖结构体、数组、指针、浮点、位域、联合、枚举等类型。
set -e
cd "$(dirname "$0")"
CC=${CC:-arm-none-eabi-gcc}
"$CC" -mcpu=cortex-m4 -mthumb -mfloat-abi=soft -O0 -g -gdwarf-4 -ffreestanding -nostdlib -nostartfiles \
  -Wall -Wextra -Werror -T types.ld -o types.elf types.c

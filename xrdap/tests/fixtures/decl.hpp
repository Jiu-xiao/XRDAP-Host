/* decl-a.cpp 和 decl-b.cpp 共用的类。虚函数 Port::Poll 定义在 decl-b.cpp，GCC 只在那个编译单元
 * 写出完整的类型，decl-a.cpp 里的 port 变量只带类型声明。 */
#pragma once
#include <stdint.h>

class Port {
public:
  virtual void Poll();
  volatile uint32_t rx = 0;
  volatile uint32_t tx = 0;
  volatile uint8_t buf[4] = {1, 2, 3, 4};
};

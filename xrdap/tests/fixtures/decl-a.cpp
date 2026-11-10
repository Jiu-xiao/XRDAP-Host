/* 成员树的测试夹具：port 的类型在本编译单元只有声明，完整定义在 decl-b.cpp。编译命令见 build.sh。 */
#include "decl.hpp"

Port port;

extern "C" void Reset_Handler() {
  for (;;) {
    port.Poll();
  }
}

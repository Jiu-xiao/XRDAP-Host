/* Port 的虚函数定义在这里，完整的类型写在这个编译单元。 */
#include "decl.hpp"

void Port::Poll() { rx = rx + 1; }

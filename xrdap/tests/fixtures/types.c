/*
 * DWARF 类型解析的测试夹具，只用来生成 types.elf，不在目标上运行。
 * 编译命令见同目录的 build.sh。全部变量放在 .data，初值由 ELF 带出，
 * 模拟探针按 ELF 的段内容填内存，测试据此核对读出的值。
 */
#include <stdbool.h>
#include <stdint.h>

typedef struct
{
  float kp;
  float ki;
  float kd;
  float out;
  int32_t integral;
} pid_ctrl_t;

enum mode
{
  MODE_IDLE = 0,
  MODE_RUN = 1,
  MODE_FAULT = 2,
};

struct flags
{
  uint8_t ready : 1;
  uint8_t error : 3;
  uint8_t raw;
};

struct motor
{
  int16_t speed;
  uint16_t current;
  enum mode mode;
  pid_ctrl_t *pid;
  double angle;
  int8_t temps[3];
  struct flags flags;
};

typedef struct motor motor_t;

struct node
{
  int32_t value;
  struct node *next;
};

union word
{
  uint32_t u;
  float f;
  uint8_t bytes[4];
};

struct opaque;

struct state
{
  int32_t speed;
  uint32_t count;
  float voltage;
  bool enabled;
  struct
  {
    uint16_t lo;
    uint16_t hi;
  } pair;
};

pid_ctrl_t pids[2] = {
    {1.5f, 0.25f, -0.125f, 12.5f, -1000},
    {2.0f, 0.5f, 0.0f, -3.75f, 77},
};

motor_t motor[2] = {
    {-1200, 350, MODE_RUN, &pids[0], 1.25, {-5, 20, 31}, {1, 2, 0x5A}},
    {800, 120, MODE_FAULT, &pids[1], -0.5, {10, -20, 30}, {0, 0, 0x33}},
};

struct state state = {-42, 123456u, 3.3f, true, {0x1111, 0x2222}};

uint32_t counter = 12345u;
int8_t s8 = -7;
uint64_t big = 0x1122334455667788ull;
int64_t neg64 = -5;
float gain = 1.5f;
double ratio = 0.25;
int32_t matrix[2][3] = {{1, 2, 3}, {4, 5, 6}};
uint32_t *ptr = &counter;
motor_t *active = &motor[1];
pid_ctrl_t *null_pid = 0;
union word word = {0x3FC00000u};
struct opaque *opaque_p = (struct opaque *)&counter;
volatile const uint16_t cv = 0xBEEF;
static uint16_t file_static = 7;

struct node nodes[3] = {
    {10, &nodes[1]},
    {20, &nodes[2]},
    {30, 0},
};
struct node *head = &nodes[0];

void Reset_Handler(void);

__attribute__((section(".isr_vector"), used)) static void (*const vectors[2])(void) = {
    (void (*)(void))0x20008000u,
    Reset_Handler,
};

void Reset_Handler(void)
{
  file_static++;
  for (;;)
  {
  }
}

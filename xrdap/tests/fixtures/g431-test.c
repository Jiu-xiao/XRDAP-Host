/*
 * STM32G431KB test fixture for the XR-DAP vendor READ/WRITE/sampling commands.
 * All interesting variables live in .fixed at the bottom of SRAM, so the host can
 * resolve them from the symbol table; values are set at runtime and never move.
 */
#include <stdint.h>

#define REG32(a) (*(volatile uint32_t *)(a))
#define SYST_CSR REG32(0xE000E010)
#define SYST_RVR REG32(0xE000E014)
#define SYST_CVR REG32(0xE000E018)
#define SCB_VTOR REG32(0xE000ED08)
#define DBGMCU_CR REG32(0xE0042004)

extern uint32_t _estack, _sfixed, _efixed, _sbss, _ebss;

void Reset_Handler(void);
void SysTick_Handler(void);
void Default_Handler(void);
int main(void);

__attribute__((section(".isr_vector"), used))
static void (*const vectors[16])(void) = {
    (void (*)(void))&_estack,
    Reset_Handler,
    Default_Handler,
    Default_Handler,
    Default_Handler,
    Default_Handler,
    Default_Handler,
    0, 0, 0, 0,
    Default_Handler,
    0,
    0,
    Default_Handler,
    SysTick_Handler,
};

/* 1/2/4/8-byte variables. */
__attribute__((section(".fixed"), used)) volatile uint8_t v_u8;
__attribute__((section(".fixed"), used)) volatile uint16_t v_u16;
__attribute__((section(".fixed"), used)) volatile uint32_t v_u32;
__attribute__((section(".fixed"), used, aligned(8))) volatile uint64_t v_u64;

/* Three-level pointer chain: chain_p1 -> chain_p2 -> chain_p3 -> chain_final. */
__attribute__((section(".fixed"), used)) volatile uint32_t chain_final;
__attribute__((section(".fixed"), used)) volatile uint32_t *chain_p3;
__attribute__((section(".fixed"), used)) volatile uint32_t *chain_p2;
__attribute__((section(".fixed"), used)) volatile uint32_t *chain_p1;

/* Null and invalid pointers. */
__attribute__((section(".fixed"), used)) volatile uint32_t *null_p1;
__attribute__((section(".fixed"), used)) volatile uint32_t *bad_p1;

/* Free-running counters, driven by SysTick at 1 kHz. */
__attribute__((section(".fixed"), used, aligned(8))) volatile uint32_t cnt_a;
__attribute__((section(".fixed"), used)) volatile uint32_t cnt_b;
__attribute__((section(".fixed"), used)) volatile uint32_t tick;
__attribute__((section(".fixed"), used, aligned(8))) volatile uint64_t cnt64;

/* A block the host can overwrite and verify with OpenOCD. */
__attribute__((section(".fixed"), used, aligned(4))) volatile uint32_t rw_block[8];

void SysTick_Handler(void)
{
  ++tick;
  ++cnt_a;
  if ((tick % 10U) == 0U)
  {
    ++cnt_b;
  }
  ++cnt64;
}

int main(void)
{
  for (uint32_t *p = &_sfixed; p < &_efixed; ++p)
  {
    *p = 0U;
  }
  for (uint32_t *p = &_sbss; p < &_ebss; ++p)
  {
    *p = 0U;
  }

  v_u8 = 0xA5U;
  v_u16 = 0x1234U;
  v_u32 = 0x89ABCDEFU;
  v_u64 = 0x0123456789ABCDEFULL;

  chain_final = 0xCAFEBABEU;
  chain_p3 = &chain_final;
  chain_p2 = (volatile uint32_t *)&chain_p3;
  chain_p1 = (volatile uint32_t *)&chain_p2;

  null_p1 = (volatile uint32_t *)0U;
  bad_p1 = (volatile uint32_t *)0xFFFFF000U;

  for (unsigned i = 0; i < 8U; ++i)
  {
    rw_block[i] = 0x11111111U * (i + 1U);
  }

  cnt_a = 0U;
  cnt_b = 0U;
  tick = 0U;
  cnt64 = 0U;

  SYST_RVR = 16000U - 1U; /* 16 MHz HSI -> 1 kHz */
  SYST_CVR = 0U;
  SYST_CSR = 7U; /* CLKSOURCE | TICKINT | ENABLE */
  DBGMCU_CR |= 1U; /* DBG_SLEEP: keep debug access while the core is in WFI */

  for (;;)
  {
    __asm volatile("nop");
  }
}

void Reset_Handler(void)
{
  SCB_VTOR = 0x08000000U;
  main();
  for (;;)
  {
  }
}

void Default_Handler(void)
{
  for (;;)
  {
  }
}

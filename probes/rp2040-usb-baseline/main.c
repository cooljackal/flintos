// SPDX-License-Identifier: Apache-2.0
// Vendor-only baseline: no FlintOS clock, USB, scheduler, or reset code.
#include "hardware/uart.h"
#include "hardware/structs/usb.h"
#include "pico/bootrom.h"
#include "pico/stdlib.h"
#include "pico/stdio_usb.h"
#include <stdio.h>

int main(void) {
    uart_init(uart0, 115200);
    gpio_set_function(0, GPIO_FUNC_UART);
    gpio_set_function(1, GPIO_FUNC_UART);
    gpio_init(24); // Pico's onboard VBUS sense; not a header signal.
    gpio_set_dir(24, GPIO_IN);
    stdio_usb_init();
    absolute_time_t next = get_absolute_time();
    for (;;) {
        if (uart_is_readable(uart0) && uart_getc(uart0) == 'B') {
            uart_puts(uart0, "VENDOR ENTERING ROM BOOTSEL\r\n");
            uart_tx_wait_blocking(uart0);
            reset_usb_boot(0, 0);
        }
        int c = getchar_timeout_us(1000);
        if (c >= 0) putchar_raw(c);
        if (absolute_time_diff_us(get_absolute_time(), next) <= 0) {
            char status[128];
            snprintf(status, sizeof(status), "VENDOR USB connected=%u vbus=%u sie=%08lx sof=%lu\r\n",
                     stdio_usb_connected(), gpio_get(24),
                     (unsigned long)usb_hw->sie_status, (unsigned long)usb_hw->sof_rd);
            uart_puts(uart0, status);
            next = make_timeout_time_ms(1000);
        }
    }
}

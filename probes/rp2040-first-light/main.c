// SPDX-License-Identifier: Apache-2.0

#include "hardware/gpio.h"
#include "hardware/uart.h"
#include "pico/stdlib.h"

enum {
    WIO_USER_LED_GPIO = 13,
    PROBE_UART_TX_GPIO = 0,
    PROBE_UART_RX_GPIO = 1,
    PROBE_UART_BAUD = 115200,
};

static const char marker[] = "FLINTOS-RP2040-FIRST-LIGHT\r\n";

int main(void) {
    gpio_init(WIO_USER_LED_GPIO);
    gpio_set_dir(WIO_USER_LED_GPIO, GPIO_OUT);

    uart_init(uart0, PROBE_UART_BAUD);
    gpio_set_function(PROBE_UART_TX_GPIO, GPIO_FUNC_UART);
    gpio_set_function(PROBE_UART_RX_GPIO, GPIO_FUNC_UART);
    uart_puts(uart0, marker);

    for (;;) {
        gpio_put(WIO_USER_LED_GPIO, 1);
        sleep_ms(250);
        gpio_put(WIO_USER_LED_GPIO, 0);
        sleep_ms(750);
    }
}

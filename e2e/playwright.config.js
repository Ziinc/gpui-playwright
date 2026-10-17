// Stock @playwright/test driving the todo example through gpui-playwright.
// No browser is launched: the app itself is the Playwright server.
const { defineConfig } = require('@playwright/test');

const port = process.env.GPUI_PLAYWRIGHT_PORT ?? '9333';
// Without a display GPUI runs headless: everything works except screenshots.
const headless =
  process.platform === 'linux' && !process.env.DISPLAY && !process.env.WAYLAND_DISPLAY;

module.exports = defineConfig({
  testDir: './tests',
  grepInvert: headless ? /@screenshot/ : undefined,
  // One app, one window: tests share it and run one at a time.
  workers: 1,
  fullyParallel: false,
  reporter: process.env.CI ? [['list'], ['html', { open: 'never' }]] : 'list',
  expect: {
    timeout: 5000,
    // Font rasterization differs slightly between machines.
    toHaveScreenshot: { maxDiffPixelRatio: 0.02 },
  },
  use: {
    connectOptions: { wsEndpoint: `ws://127.0.0.1:${port}/` },
    viewport: { width: 560, height: 480 },
  },
  webServer: {
    command: 'cargo run -p todo',
    cwd: '..',
    url: `http://127.0.0.1:${port}/`,
    env: { GPUI_PLAYWRIGHT: port },
    reuseExistingServer: !process.env.CI,
    timeout: 600_000,
    stdout: 'pipe',
  },
});

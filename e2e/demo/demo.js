// Drives the to-do example the way a test would, slowly enough to watch.
// record.sh films it for the README; each step's code becomes its caption.
// Prints one JSON line per step: {"at": Unix time in seconds, "code": "..."}.
const { chromium } = require('@playwright/test');

const port = process.env.GPUI_PLAYWRIGHT_PORT ?? '9333';
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

(async () => {
  const browser = await chromium.connect(`ws://127.0.0.1:${port}/`);
  const page = await browser.newPage();
  const step = async (code, run) => {
    console.log(JSON.stringify({ at: Date.now() / 1000, code }));
    await run();
    await pause(900);
  };

  await pause(800);
  await step("await page.getByRole('textbox').fill('Buy milk')", () =>
    page.getByRole('textbox').fill('Buy milk'),
  );
  await step("await page.getByRole('button', { name: 'Add' }).click()", () =>
    page.getByRole('button', { name: 'Add' }).click(),
  );
  await step("await page.keyboard.type('Walk the dog')", async () => {
    await page.getByRole('textbox').click();
    await page.keyboard.type('Walk the dog', { delay: 60 });
  });
  await step("await page.keyboard.press('Enter')", () => page.keyboard.press('Enter'));
  await step("await page.getByRole('checkbox', { name: 'Buy milk' }).check()", () =>
    page.getByRole('checkbox', { name: 'Buy milk' }).check(),
  );
  await step("await expect(page.getByText('2 of 4 left')).toBeVisible()", () =>
    page.getByText('2 of 4 left').waitFor(),
  );
  await step("await page.getByRole('button', { name: 'Delete' }).first().click()", () =>
    page.getByRole('button', { name: 'Delete' }).first().click(),
  );
  await step("await page.emulateMedia({ colorScheme: 'dark' })", () =>
    page.emulateMedia({ colorScheme: 'dark' }),
  );
  await pause(600);
  console.log(JSON.stringify({ at: Date.now() / 1000, code: null }));
  await browser.close();
})();

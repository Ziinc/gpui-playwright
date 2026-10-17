// Failing assertions must fail, with Playwright's own messages, on every
// supported client version. Each client version reports failures differently.
const { test, expect } = require('@playwright/test');

async function failure(assertion) {
  try {
    await assertion;
  } catch (error) {
    // Drop the terminal colors Playwright adds to matcher messages.
    return error.message.replace(/\u001b\[[0-9;]*m/g, '');
  }
  throw new Error('expected the assertion to fail');
}

test('failing matchers report what they received', async ({ page }) => {
  const first = page.getByRole('checkbox').first();
  expect(await failure(expect(first).toHaveText('nope', { timeout: 500 }))).toContain('Read the docs');
  expect(await failure(expect(page.getByRole('checkbox')).toHaveCount(7, { timeout: 500 }))).toContain('2');
  expect(await failure(expect(first).not.toBeVisible({ timeout: 500 }))).toContain('not.toBeVisible');
  expect(await failure(expect(page).toHaveTitle('nope', { timeout: 500 }))).toContain('Todos');
});

test('matchers are strict', async ({ page }) => {
  expect(await failure(expect(page.getByRole('checkbox')).toBeVisible({ timeout: 500 }))).toContain(
    'strict mode violation',
  );
});

test('actions time out', async ({ page }) => {
  expect(await failure(page.getByRole('button', { name: 'Missing' }).click({ timeout: 500 }))).toContain(
    'Timeout 500ms exceeded',
  );
});

test('a changed screen fails toHaveScreenshot', { tag: '@screenshot' }, async ({ page }) => {
  await page.getByRole('button', { name: 'Dark' }).click();
  try {
    const add = page.getByRole('button', { name: 'Add' });
    expect(await failure(expect(add).toHaveScreenshot('add-button.png', { timeout: 1000 }))).toContain(
      'toHaveScreenshot',
    );
  } finally {
    await page.getByRole('button', { name: 'Light' }).click();
  }
});

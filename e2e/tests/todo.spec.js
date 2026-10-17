const { test, expect } = require('@playwright/test');

test('starts from the seeded list', async ({ page }) => {
  await expect(page).toHaveTitle('Todos');
  await expect(page.getByRole('checkbox')).toHaveCount(2);
  await expect(page.getByRole('checkbox', { name: 'Read the docs' })).toBeChecked();
  await expect(page.getByRole('checkbox', { name: 'Write a Playwright test' })).not.toBeChecked();
  await expect(page.getByText('1 of 2 left')).toBeVisible();
});

test('adds a todo with the button', async ({ page }) => {
  await page.getByRole('textbox').fill('Buy milk');
  await expect(page.getByRole('textbox')).toHaveValue('Buy milk');
  await page.getByRole('button', { name: 'Add' }).click();
  await expect(page.getByRole('checkbox', { name: 'Buy milk' })).toBeVisible();
  await expect(page.getByRole('textbox')).toHaveValue('');
  await expect(page.getByText('2 of 3 left')).toBeVisible();
});

test('adds a todo with Enter', async ({ page }) => {
  const input = page.getByRole('textbox');
  await input.click();
  await page.keyboard.type('Walk the dog');
  await page.keyboard.press('Enter');
  await expect(page.getByRole('checkbox')).toHaveText(['Read the docs', 'Write a Playwright test', 'Walk the dog']);
});

test('checks and unchecks todos', async ({ page }) => {
  const todo = page.getByRole('checkbox', { name: 'Write a Playwright test' });
  await todo.check();
  await expect(todo).toBeChecked();
  await expect(page.getByText('0 of 2 left')).toBeVisible();
  await todo.uncheck();
  await expect(todo).not.toBeChecked();
  expect(await todo.isChecked()).toBe(false);
});

test('deletes a todo', async ({ page }) => {
  await page.getByRole('button', { name: 'Delete' }).first().click();
  await expect(page.getByRole('checkbox', { name: 'Read the docs' })).toHaveCount(0);
  await expect(page.getByRole('checkbox')).toHaveCount(1);
});

test('reports strict mode violations', async ({ page }) => {
  const error = await page.getByRole('button', { name: 'Delete' }).click({ timeout: 1000 }).catch((e) => e);
  expect(error.message).toContain('strict mode violation');
  expect(error.message).toContain('resolved to 2 elements');
});

test('locators by test id and filters', async ({ page }) => {
  await expect(page.getByTestId('remaining')).toHaveText('1 of 2 left');
  const unchecked = page.getByRole('checkbox').filter({ hasText: 'Playwright' });
  await expect(unchecked).toHaveCount(1);
  await expect(page.getByRole('checkbox').nth(1)).toHaveAccessibleName('Write a Playwright test');
  await expect(page.getByRole('checkbox').last()).toContainText('Playwright');
  expect(await page.getByRole('checkbox').allTextContents()).toEqual(['Read the docs', 'Write a Playwright test']);
  await page.getByLabel('What needs to be done?').pressSequentially('abc');
  await expect(page.getByRole('textbox')).toHaveValue('abc');
});

test('aria snapshot', async ({ page }) => {
  await expect(page.getByRole('checkbox').first()).toHaveAttribute('aria-checked', 'true');
  const snapshot = await page.getByRole('checkbox').first().ariaSnapshot();
  expect(snapshot).toContain('checkbox "Read the docs" [checked]');
});

test('screenshots', { tag: '@screenshot' }, async ({ page }) => {
  const shot = await page.screenshot();
  expect(shot.length).toBeGreaterThan(1000);
  await expect(page.getByRole('button', { name: 'Add' })).toHaveScreenshot('add-button.png');
});

test('color scheme follows emulateMedia', { tag: '@screenshot' }, async ({ page }) => {
  const add = page.getByRole('button', { name: 'Add' });
  await page.emulateMedia({ colorScheme: 'dark' });
  await expect(page.getByRole('button', { name: 'Light' })).toBeVisible();
  await expect(add).not.toHaveScreenshot('add-button.png');
  await page.emulateMedia({ colorScheme: 'light' });
  await expect(add).toHaveScreenshot('add-button.png');
});

test('keyboard shortcuts and mouse', async ({ page }) => {
  const input = page.getByRole('textbox');
  await input.fill('typo');
  await input.press('ControlOrMeta+a');
  await page.keyboard.press('Backspace');
  await expect(input).toHaveValue('');
  const box = await page.getByRole('checkbox', { name: 'Write a Playwright test' }).boundingBox();
  await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
  await expect(page.getByText('0 of 2 left')).toBeVisible();
});

test('goto runs the app route', async ({ page }) => {
  await page.goto('todo://clear-completed');
  await expect(page).toHaveURL('todo://clear-completed');
  await expect(page.getByRole('checkbox')).toHaveText(['Write a Playwright test']);
  await expect(page.goto('todo://nowhere')).rejects.toThrow('no route for todo://nowhere');
});

test('finds painted text, placeholders, titles and alt text', async ({ page }) => {
  await expect(page.getByText('Todos', { exact: true })).toBeVisible();
  await page.getByPlaceholder('What needs to be done?').fill('Water plants');
  await page.getByRole('button', { name: 'Add' }).click();
  await expect(page.getByText('Water plants')).toBeVisible();
  await expect(page.getByTitle('Switch theme')).toBeVisible();
  await expect(page.getByAltText('Ada Lovelace')).toBeVisible();
  await expect(page.getByRole('img', { name: 'Ada Lovelace' })).toHaveCount(1);
});

test('evaluate runs scripts against the page', async ({ page }) => {
  expect(await page.evaluate('1 + 1')).toBe(2);
  expect(await page.evaluate(() => document.title)).toBe('Todos');
  expect(await page.evaluate((n) => n * 2, 21)).toBe(42);
  const first = page.getByRole('checkbox').first();
  expect(await first.evaluate((el) => [el.textContent, el.checked])).toEqual(['Read the docs', true]);
  expect(await page.getByRole('checkbox').evaluateAll((els) => els.length)).toBe(2);
  expect(await page.evaluate(() => todos().map((t) => t.title))).toEqual(['Read the docs', 'Write a Playwright test']);
  const handle = await page.evaluateHandle(() => ({ left: todos().filter((t) => !t.done).length }));
  expect(await handle.jsonValue()).toEqual({ left: 1 });
  await expect(page.evaluate(() => { throw new Error('boom'); })).rejects.toThrow('boom');

  const added = page.waitForFunction(() => todos().length === 3);
  await page.getByRole('textbox').fill('Ship it');
  await page.getByRole('button', { name: 'Add' }).click();
  await added;
});

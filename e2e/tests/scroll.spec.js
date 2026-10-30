const { test, expect } = require('@playwright/test');

// `todo://notes` shows a short scrolling panel under the list: "Note" sits at
// its bottom edge with only a sliver showing, "Tag" is entirely below the fold.
// Actions must bring the target into view and act on it, never on whatever
// already has focus.

test.beforeEach(async ({ page }) => {
  await page.goto('todo://notes');
  const title = page.getByRole('textbox', { name: 'What needs to be done?' });
  await title.fill('keep me');
  await expect(title).toBeFocused();
});

test('fill reaches a textbox only partly in view, not the focused one', async ({ page }) => {
  await page.getByRole('textbox', { name: 'Note' }).fill('noted');
  await expect(page.getByRole('textbox', { name: 'Note' })).toHaveValue('noted');
  await expect(page.getByRole('textbox', { name: 'What needs to be done?' })).toHaveValue('keep me');
});

test('fill scrolls a textbox below the fold into view', async ({ page }) => {
  await page.getByRole('textbox', { name: 'Tag' }).fill('urgent');
  await expect(page.getByRole('textbox', { name: 'Tag' })).toHaveValue('urgent');
  await expect(page.getByRole('textbox', { name: 'Tag' })).toBeInViewport();
  await expect(page.getByRole('textbox', { name: 'What needs to be done?' })).toHaveValue('keep me');
});

test('press and type go to the target after scrolling', async ({ page }) => {
  const tag = page.getByRole('textbox', { name: 'Tag' });
  // The panel's inputs outlive a context; start from empty.
  await tag.clear();
  await page.getByRole('textbox', { name: 'What needs to be done?' }).focus();
  await tag.pressSequentially('ab');
  await tag.press('Backspace');
  await expect(tag).toHaveValue('a');
  await expect(page.getByRole('textbox', { name: 'What needs to be done?' })).toHaveValue('keep me');
});

test('fill scrolls back up to a textbox above the fold', async ({ page }) => {
  await page.getByRole('textbox', { name: 'Tag' }).fill('urgent');
  await page.getByRole('textbox', { name: 'Note' }).fill('noted');
  await expect(page.getByRole('textbox', { name: 'Note' })).toHaveValue('noted');
  await expect(page.getByRole('textbox', { name: 'Note' })).toBeInViewport();
  await expect(page.getByRole('textbox', { name: 'Tag' })).toHaveValue('urgent');
});

test('fill fails on a textbox nothing can scroll into view, without typing elsewhere', async ({ page }) => {
  const error = await page
    .getByRole('textbox', { name: 'Hidden' })
    .fill('lost', { timeout: 1000 })
    .catch((e) => e);
  expect(error.message).toContain('Timeout 1000ms exceeded');
  expect(error.message).toContain('element is outside of the viewport');
  await expect(page.getByRole('textbox', { name: 'What needs to be done?' })).toHaveValue('keep me');
});

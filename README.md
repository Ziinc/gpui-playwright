# gpui-playwright

Drive [GPUI](https://gpui.rs) and [GPUI Kit](https://gpui-kit.com) apps with the
Playwright clients you already use. gpui-playwright runs a Playwright server
inside your app: `@playwright/test`, `playwright-python`, `playwright-java` and
`playwright-dotnet` connect to it with `connect()` / `connectOptions` and treat
each app window as a page. No client SDK, no browser download.

```js
// playwright.config.js
module.exports = {
  use: { connectOptions: { wsEndpoint: 'ws://127.0.0.1:9333/' } },
  webServer: {
    command: 'cargo run -p my-app',
    url: 'http://127.0.0.1:9333/',
    env: { GPUI_PLAYWRIGHT: '9333' },
  },
};

// tests/app.spec.js
test('adds a todo', async ({ page }) => {
  await page.getByRole('textbox').fill('Buy milk');
  await page.getByRole('button', { name: 'Add' }).click();
  await expect(page.getByRole('checkbox', { name: 'Buy milk' })).not.toBeChecked();
  await expect(page).toHaveScreenshot();
});
```

## Add it to an app

```toml
[features]
playwright = ["dep:gpui-playwright"]

[dependencies]
gpui-playwright = { git = "https://github.com/Ziinc/gpui-playwright", optional = true }
```

```rust
gpui_kit::application().run(|cx| {
    gpui_kit::init(cx);
    open_main_window(cx);

    #[cfg(feature = "playwright")]
    if let Some(server) = gpui_playwright::Server::from_env() {
        server
            // Optional: give each Playwright context a clean slate.
            .on_new_context(|cx| reset_app_state(cx))
            // Optional: open a window per `context.newPage()`.
            .on_new_page(|cx| open_main_window(cx))
            // Optional: honour `colorScheme` and `page.emulateMedia()`.
            .on_color_scheme(|scheme, cx| apply_theme(scheme, cx))
            // Optional: route `page.goto(url)`, such as a deep link.
            .on_navigate(|url, window, cx| open_route(url, window, cx))
            .start(cx)
            .expect("start gpui-playwright");
    }
});
```

The server only starts when `GPUI_PLAYWRIGHT` is set (`9333` or
`127.0.0.1:9333`). `GPUI_PLAYWRIGHT_TOKEN` adds a secret path segment
(`ws://127.0.0.1:9333/<token>`), and `GPUI_PLAYWRIGHT_ENDPOINT_FILE` writes the
endpoint to a file once listening. A plain HTTP request to the port answers
`200`, which is what `webServer.url` waits for.

gpui-playwright enables gpui-kit's `test-support` feature, which observes the
elements Playwright selects. Keep it behind a cargo feature so release builds
leave it out.

## What maps onto a desktop app

Elements come from gpui-kit's per-frame observation: role, accessible label,
value, checked/disabled/focused/expanded/selected state, bounds and the GPUI
element id. gpui-kit components register themselves; give your own elements an
id and call `.test_support()` (plus `.aria_label(...)`) to make them
selectable.

| Playwright | Support |
| --- | --- |
| `getByRole` (name, exact, checked, disabled, expanded, selected, includeHidden, level, pressed) | Yes |
| `getByLabel`, `getByText`, `getByTestId` (the GPUI element id) | Yes; an element's text is its label until gpui-kit observes rendered text |
| `getByTitle`, `getByPlaceholder`, `getByAltText` | Match nothing yet: gpui-kit does not observe those attributes |
| `locator('#id')`, `[attr=value]`, `filter({ has, hasNot, hasText, hasNotText, visible })`, `and`, `or`, `first`, `last`, `nth`, chaining | Yes |
| Other CSS and XPath selectors | Error that names the selector |
| `click`, `dblclick`, `tap`, `hover`, `check`, `uncheck`, `setChecked`, `fill`, `clear`, `press`, `pressSequentially`, `focus` | Yes, with actionability waits and strict mode |
| `page.keyboard.*`, `page.mouse.*`, `page.touchscreen.tap` | Yes |
| `textContent`, `innerText`, `inputValue`, `getAttribute`, `boundingBox`, `isVisible`/`isEnabled`/`isChecked`/..., `count`, `all`, `allInnerTexts`, `allTextContents`, `ariaSnapshot` | Yes |
| `expect(locator)`: `toBeVisible`, `toBeHidden`, `toBeEnabled`, `toBeDisabled`, `toBeEditable`, `toBeFocused`, `toBeChecked`, `toBeEmpty`, `toBeAttached`, `toBeInViewport`, `toHaveText`, `toContainText`, `toHaveValue`, `toHaveCount`, `toHaveAccessibleName`, `toHaveAccessibleDescription`, `toHaveRole`, `toHaveId`, `toHaveAttribute` | Yes |
| `expect(page).toHaveTitle` | Yes (the window title) |
| `page.goto`, `page.url()`, `expect(page).toHaveURL` | With `on_navigate`: the app routes the URL |
| `page.screenshot`, `locator.screenshot`, `toHaveScreenshot` (clip, mask, maskColor, threshold, maxDiffPixels) | Yes |
| `page.setViewportSize`, context `viewport`, `colorScheme`, `page.emulateMedia({ colorScheme })`, `bringToFront`, `close` | Yes |
| Tracing (`trace: 'on'`), `route`, cookies, storage state | Accepted as no-ops; traces are empty |
| `reload`, `goBack`, `setContent`, `evaluate`, `waitForFunction`, network, downloads, frames | Clear "not supported by GPUI apps" errors |

The server speaks Playwright **1.58 through 1.64** (`@playwright/test`,
`playwright` for Python, or the matching Java/.NET release) and answers each
client in its own version's protocol. CI runs the suite against every one.
Other versions are refused at connect with Playwright's own version-mismatch
message.

## Pages and contexts

- `browser.newContext()` runs `on_new_context`, so tests start from a known state.
  Without it, contexts share the running app's state.
- `context.newPage()` runs `on_new_page` and closes that window again when the
  page or context closes. Without it, the page adopts the first app window no
  other page holds.
- A context `viewport` (the `@playwright/test` default is 1280×720) resizes the
  window; set `use.viewport` to your app's natural size.
- Closing a window closes its page.

## Platforms

Screenshots use GPUI's `render_to_image` where the platform implements it, and
read the window back from the X server on X11. Without a display GPUI starts
headless: the server renders frames itself, so locators, actions and `expect`
work, but screenshots need a visible window. On Linux CI, run screenshot tests
under Xvfb with a software Vulkan driver (see `.github/workflows/ci.yml`).

## Chaos testing

`e2e/chaos/chaos.js` drives a running app with malformed protocol traffic,
connection churn and a seeded random walk of locators and actions, and checks
the app's invariants as it goes. A finding names the seed that reproduces it.

```sh
GPUI_PLAYWRIGHT=9333 cargo run -p todo &
node e2e/chaos/chaos.js --seed 42 --steps 300
```

## Repository

- `crates/server` – the `gpui-playwright` crate: WebSocket server, Playwright
  object model, input, expect and screenshots.
- `crates/selectors` – `gpui-playwright-selectors`: Playwright's selector
  language (`internal:role=…`, `internal:label=…`, filters, `nth`, `>>`) over a
  plain element tree, with no GPUI dependency.
- `examples/todo` – a gpui-kit to-do app with the server enabled.
- `e2e` – a stock `@playwright/test` suite for the example.
- `examples/python` – the same app driven from `playwright-python`.

```sh
cargo test --workspace
cd e2e && npm ci && xvfb-run -a npx playwright test
```

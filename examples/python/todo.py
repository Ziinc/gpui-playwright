"""Drive the todo example from Python with the stock `playwright` package.

    GPUI_PLAYWRIGHT=9333 cargo run -p todo &
    pip install playwright==1.64.0
    python examples/python/todo.py ws://127.0.0.1:9333/

No browser is installed or launched: the app is the Playwright server.
"""

import sys

from playwright.sync_api import expect, sync_playwright

endpoint = sys.argv[1] if len(sys.argv) > 1 else "ws://127.0.0.1:9333/"

with sync_playwright() as p:
    browser = p.chromium.connect(endpoint)
    page = browser.new_context().new_page()

    page.get_by_role("textbox").fill("Feed the cat")
    page.get_by_role("button", name="Add").click()
    expect(page.get_by_role("checkbox", name="Feed the cat")).not_to_be_checked()

    page.get_by_role("checkbox", name="Feed the cat").check()
    expect(page.get_by_text("1 of 3 left")).to_be_visible()

    print(page.get_by_role("checkbox").all_inner_texts())
    page.screenshot(path="todo.png")
    browser.close()

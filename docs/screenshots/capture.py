"""Take the documentation's screenshots from the app's own pages, with the
made-up card in mock.js standing in for a real one.

Start the frontend first (`npm run dev` in app/), then from the repo root:

    uv run --no-project --with playwright python -I docs/screenshots/capture.py

It drives WebKit, the engine Tauri uses on macOS, and writes the images to
docs/images/. Playwright needs its WebKit browser: `playwright install webkit`.
"""

import pathlib

from playwright.sync_api import sync_playwright

HERE = pathlib.Path(__file__).parent
IMAGES = HERE.parent / "images"
URL = "http://localhost:1420/"


def tab(page, name):
    page.locator("nav .tabs button", has_text=name).first.click()
    page.wait_for_timeout(500)


with sync_playwright() as p:
    browser = p.webkit.launch()
    page = browser.new_page(viewport={"width": 1100, "height": 760}, device_scale_factor=2)
    page.add_init_script(path=str(HERE / "mock.js"))
    page.emulate_media(color_scheme="light")
    page.goto(URL)
    page.wait_for_selector("li[data-slot]")
    page.wait_for_timeout(500)

    # The Library with one reel picked, so its toolbar shows.
    page.locator('li[data-slot="2"] .name').click()
    page.mouse.move(5, 5)
    page.screenshot(path=str(IMAGES / "library.png"))

    page.locator('li[data-slot="2"] button', has_text="Edit").click()
    page.wait_for_selector("canvas")
    page.wait_for_timeout(800)
    page.screenshot(path=str(IMAGES / "splices.png"))
    page.locator(".tabs-inline button", has_text="Markers").click()
    page.wait_for_timeout(300)
    page.screenshot(path=str(IMAGES / "splices-markers.png"))
    page.locator(".tabs-inline button", has_text="Edit").click()

    tab(page, "Build")
    page.evaluate("window.nextOpen = ['/x/harbour 01.wav', '/x/harbour 02.wav', '/x/bell, close.wav']")
    page.get_by_role("button", name="Add files…").click()
    page.wait_for_timeout(500)
    page.screenshot(path=str(IMAGES / "build.png"))

    tab(page, "Sets")
    page.screenshot(path=str(IMAGES / "sets.png"))

    tab(page, "options.txt")
    page.screenshot(path=str(IMAGES / "options.png"))
    browser.close()

print("Wrote", ", ".join(sorted(f.name for f in IMAGES.glob("*.png"))))

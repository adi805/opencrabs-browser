# Evidence: re-opening sites that had failed before

Captured 4 Nov 2026 08:54 to 08:58 WIB through the `browserd` daemon on
`127.0.0.1:9224` (a new tab per site, `Page.enable` + `Network.enable` +
`Runtime.evaluate` + `Page.captureScreenshot`, the tab closed afterwards).
Production port 9222 was read read-only before and after: 0 tabs, it received no
command at all.

The site that had failed before was the Google consent page: `browser_click` and
`browser_act` timed out, `browser_eval` plus `.click()` worked, and
`querySelectorAll('[role="checkbox"]')` returned 0 because the boxes are custom
elements. That is why this page was re-tested first.

## Results

| Site | Document status | Final URL | Title | DOM signal | Seconds | Correct category |
|---|---|---|---|---|---|---|
| `google.com/search?q=...` | 200 then **429** | `/sorry/index?...` | empty | `recaptcha/enterprise` frame | 25.1 (hit the limit) | `blok_anti_bot` |
| `nowsecure.nl` | not captured (the tab was created already carrying the URL) | `nowsecure.nl` | `nowsecure.nl` | `captcha_dom` (hCaptcha) | 2.0 and 13.3 across two runs | `blok_anti_bot` |
| a deliberately non-existent host (a made-up `.test` name) | none | `chrome-error://chromewebdata/` | empty | none | 0.3 | `jaringan` |
| `instagram.com` | not captured | `instagram.com` | `Instagram` | none | 15.2 | `render_lambat` (borderline) or success |
| `consent.google.com/?gl=ID` | **404** | same | `Error 404 (Not Found)!!1` | none | 0.9 | not a challenge, an ordinary error page |

## Three lessons that changed the classification design

1. **Status alone is not enough, and status can go missing.** A client that creates
   a tab through `PUT /json/new?<url>` starts the navigation before
   `Network.enable` has been sent, so `Network.responseReceived` for the first
   document is never seen. `nowsecure.nl` proves it: zero document events, yet the
   page is clearly a challenge. Classification must have a path from the final URL
   and DOM signals, not from status alone.
2. **Google answers 429 at the `/sorry/index` URL, not 403, and the page text is 0
   characters.** Text-based detection alone would report "a normal page". The
   evidence that saved it: status 429 plus the URL pattern plus a reCAPTCHA frame.
3. **`chrome-error://chromewebdata/` is the strongest network evidence.** A
   Chromium error page has a 0 character body, so without checking the final URL
   the system would call it "an empty page" instead of a network failure.

## What is not measured yet

The anti-bot detection rate on real hosts (one attempt per site, not a sample), and
the behaviour of `chrome_full` under xvfb. There is no claim about either.

Raw files: `/tmp/oc-buka-{google2,cf2,dnsgagal,ig,consent}.json` and their
matching screenshots.
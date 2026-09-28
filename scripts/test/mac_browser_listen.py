"""Runs on the Mac (copied there by browser_direct_path.py): headless Chrome
signs in to the web client, listens to SOURCE on DEVICE, and prints the
stream's diagnostics, including the "Route:" line (which ICE path was
chosen). Chrome is not given microphone access, so like a phone that only
listens it hides its local address behind a .local name.

Usage: python3 mac_browser_listen.py SETTINGS_FILE
The settings file (key=value: URL, USER, PASSWORD, DEVICE, SOURCE, SECONDS)
is deleted as soon as it is read; the password is never printed.
"""
import os, sys, tempfile, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait, Select

path = sys.argv[1]
cfg = dict(l.split("=", 1) for l in open(path, encoding="utf-8").read().splitlines() if "=" in l)
os.remove(path)

opts = webdriver.ChromeOptions()
for a in ["--headless=new", "--autoplay-policy=no-user-gesture-required",
          f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-chrome-')}"]:
    opts.add_argument(a)
drv = webdriver.Chrome(options=opts)
drv.set_script_timeout(15)
try:
    w = WebDriverWait(drv, 40)
    drv.get(cfg["URL"])
    w.until(lambda d: d.find_element(By.ID, "sign-in-section").is_displayed())
    drv.find_element(By.ID, "username").send_keys(cfg["USER"])
    drv.find_element(By.ID, "password").send_keys(cfg["PASSWORD"])
    drv.find_element(By.CSS_SELECTOR, "#sign-in-form button[type=submit]").click()
    w.until(lambda d: "Connected" in d.find_element(By.ID, "connection-state").text)
    card = w.until(lambda d: next((c for c in d.find_elements(By.CSS_SELECTOR, ".device")
                                   if cfg["DEVICE"] in c.text and ": online" in c.text), None))
    # Devices are collapsed disclosures: expand this one (as Enter would).
    if not card.get_attribute("open"):
        card.find_element(By.TAG_NAME, "summary").click()
    src = Select(card.find_element(By.CSS_SELECTOR, "select[id$='-source']"))
    src.select_by_visible_text(next(o.text for o in src.options if cfg["SOURCE"] in o.text))
    card.find_element(By.CSS_SELECTOR, "form.listen button").click()
    try:
        w.until(lambda d: "Connected" in d.find_element(By.CSS_SELECTOR, "#streams li .state").text)
        time.sleep(float(cfg.get("SECONDS", "8")))
    except Exception:
        print("STREAM DID NOT CONNECT:", drv.find_element(By.ID, "streams").text.replace("\n", " | "))
    print(drv.find_element(By.ID, "diagnostics").get_attribute("textContent"))
    # Every candidate pair, for diagnosis.
    pairs = drv.execute_async_script("""
        const done = arguments[arguments.length - 1];
        const s = [...state.sessions.values()][0];
        s.pc.getStats().then(stats => {
            const byId = new Map(); stats.forEach(r => byId.set(r.id, r));
            const out = [];
            stats.forEach(r => { if (r.type === 'candidate-pair') {
                const l = byId.get(r.localCandidateId), m = byId.get(r.remoteCandidateId);
                out.push(`pair ${r.id} ${l && l.candidateType}/${m && m.candidateType} ${m && m.address}:${m && m.port} state ${r.state} nominated ${r.nominated} received ${r.packetsReceived || 0}`); } });
            stats.forEach(r => { if (r.type === 'transport') out.push('selected ' + r.selectedCandidatePairId + ' ice ' + r.iceState + ' dtls ' + r.dtlsState); });
            out.push('pc ' + s.pc.connectionState + ' ice ' + s.pc.iceConnectionState);
            done(out.join('\\n'));
        });
    """)
    print(pairs)
    drv.find_element(By.CSS_SELECTOR, "#streams li button.stop").click()
    time.sleep(1)
finally:
    drv.quit()

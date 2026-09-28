"""Checks email addresses and password reset in the web client, as a screen
reader sees it (Chrome's accessibility tree), in headless Chrome, against a
local server started for the test (temporary database) that sends its email
to a small SMTP sink in this script, so no real email is sent:

1. Creating an account asks for an email address (named, with its purpose
   as the description); leaving it empty is caught, with focus on it.
2. After creating the account, the announcement says a link was emailed,
   and a "Confirm your email address" banner (a region with a heading) is
   shown.
3. Opening the emailed link confirms the address: announced, the link's
   token leaves the address bar, and the banner goes away.
4. "Forgot your password?" moves focus to its heading; asking for a link
   gives the same answer as for any account, and the email arrives.
5. The reset link opens "Choose a new password" with focus on its heading;
   different passwords are caught; setting one signs in, announces it, and
   the old password stops working.
6. An account without an email address (made by the administrator) sees
   the "Add an email address" banner, saying an address is highly
   recommended and used only for password resets; the sign-in
   announcement says so too.
7. Adding an address needs the password (a wrong one is refused, with
   focus on the password field); afterwards the confirmation email
   arrives and the state is announced.
8. Changing a confirmed address says the old one stays in use until the
   new one is confirmed; the new one gets a link and the old one a notice.

Environment: AUDIONET_SERVER (default target/release/audionet-server.exe).
Requires: pip install selenium; Chrome.
"""
import email, os, re, socket, subprocess, sys, tempfile, threading, time
from selenium import webdriver
from selenium.webdriver.common.by import By
from selenium.webdriver.support.ui import WebDriverWait

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(os.path.dirname(HERE))
SERVER = os.environ.get("AUDIONET_SERVER", os.path.join(ROOT, "target", "release", "audionet-server.exe"))
results = []
inbox = []  # (to, message text)


def check(name, ok, detail=""):
    results.append(ok)
    print(f"{name}: {'PASS' if ok else 'FAIL'}{'' if ok or not detail else '  (' + detail + ')'}")


def free_port():
    with socket.socket() as s:
        s.bind(("127.0.0.1", 0))
        return s.getsockname()[1]


def smtp_session(conn):
    f = conn.makefile("rb")
    conn.sendall(b"220 sink ESMTP\r\n")
    to, data, in_data = None, [], False
    while True:
        line = f.readline()
        if not line:
            return
        text = line.decode("utf-8", "replace").rstrip("\r\n")
        if in_data:
            if text == ".":
                in_data = False
                inbox.append((to, "\n".join(data)))
                data = []
                conn.sendall(b"250 queued\r\n")
            else:
                data.append(text[1:] if text.startswith("..") else text)
            continue
        verb = text[:4].upper()
        if verb == "RCPT":
            to = re.search(r"<([^>]*)>", text).group(1)
        if verb == "DATA":
            in_data = True
            conn.sendall(b"354 go ahead\r\n")
        elif verb == "QUIT":
            conn.sendall(b"221 bye\r\n")
            return
        else:
            conn.sendall(b"250 ok\r\n")


def start_smtp_sink():
    srv = socket.socket()
    srv.bind(("127.0.0.1", 0))
    srv.listen()

    def serve():
        while True:
            conn, _ = srv.accept()
            threading.Thread(target=smtp_session, args=(conn,), daemon=True).start()

    threading.Thread(target=serve, daemon=True).start()
    return srv.getsockname()[1]


def wait_mail(count, timeout=10):
    end = time.time() + timeout
    while len(inbox) < count and time.time() < end:
        time.sleep(0.1)
    return inbox[count - 1] if len(inbox) >= count else (None, "")


def link_in(message, param):
    # Decode the body as a mail program would (lettre may send long lines
    # quoted-printable).
    body = email.message_from_string(message).get_payload(decode=True).decode("utf-8")
    m = re.search(r"(http://\S+/\?" + param + r"=[A-Za-z0-9_-]+)", body)
    return m.group(1) if m else None


def start_server(smtp_port):
    work = tempfile.mkdtemp(prefix="audionet-email-")
    port = free_port()
    cfg = os.path.join(work, "config.toml")
    web = os.path.join(ROOT, "web").replace("\\", "/")
    db = os.path.join(work, "audionet.db").replace("\\", "/")
    with open(cfg, "w") as f:
        f.write(f'public_url = "http://127.0.0.1:{port}"\nbind = "127.0.0.1:{port}"\n'
                f'database = "{db}"\nweb_root = "{web}"\nallow_registration = true\n'
                f'[email]\nfrom = "AudioNet <no-reply@example.com>"\nsmtp_host = "localhost"\n'
                f'smtp_port = {smtp_port}\nsmtp_security = "none"\n')
    # An account from before email addresses.
    subprocess.run([SERVER, "-c", cfg, "user", "add", "veteran"], input=b"correct horse battery\n",
                   check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    proc = subprocess.Popen([SERVER, "-c", cfg, "serve"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(100):
        try:
            socket.create_connection(("127.0.0.1", port), timeout=0.2).close()
            break
        except OSError:
            time.sleep(0.1)
    return proc, f"http://127.0.0.1:{port}"


def ax(drv, selector):
    obj = drv.execute_cdp_cmd("Runtime.evaluate", {"expression": f"document.querySelector({selector!r})"})["result"]
    n = drv.execute_cdp_cmd("Accessibility.getPartialAXTree", {"objectId": obj["objectId"], "fetchRelatives": False})["nodes"][0]
    props = {p["name"]: p["value"].get("value") for p in n.get("properties", [])}
    return n["role"]["value"], n.get("name", {}).get("value", ""), n.get("description", {}).get("value", ""), props


def focused_id(drv):
    return drv.execute_script("return document.activeElement && document.activeElement.id")


def alert_text(drv):
    WebDriverWait(drv, 5).until(lambda d: d.find_element(By.ID, "alert").text.strip())
    return drv.find_element(By.ID, "alert").text


def status_text(drv, contains):
    try:
        WebDriverWait(drv, 5).until(lambda d: contains in d.find_element(By.ID, "status").text)
    except Exception:
        pass
    return drv.find_element(By.ID, "status").text


def shown(drv, id_):
    return drv.find_element(By.ID, id_).is_displayed()


def submit(drv, form):
    drv.find_element(By.CSS_SELECTOR, f"#{form} button[type=submit]").click()


def sign_out(drv, w):
    drv.find_element(By.ID, "sign-out").click()
    w.until(lambda d: shown(d, "sign-in-section"))


opts = webdriver.ChromeOptions()
for a in ["--headless=new", f"--user-data-dir={tempfile.mkdtemp(prefix='audionet-email-chrome-')}"]:
    opts.add_argument(a)
drv = webdriver.Chrome(options=opts)
proc = None
try:
    drv.execute_cdp_cmd("Accessibility.enable", {})
    w = WebDriverWait(drv, 30)
    proc, base = start_server(start_smtp_sink())
    drv.get(base)
    w.until(lambda d: shown(d, "sign-in-section"))
    w.until(lambda d: shown(d, "forgot-offer"))

    # 1. Creating an account needs an address.
    drv.find_element(By.ID, "show-create-account").click()
    w.until(lambda d: shown(d, "create-account-section"))
    role, name, desc, props = ax(drv, "#new-email")
    check("email field named, with its purpose",
          name == "Email address" and "only to reset your password" in desc and props.get("required") is True,
          f"{role} {name!r} {desc!r} {props}")
    drv.find_element(By.ID, "new-username").send_keys("listener")
    for fid in ("new-password", "new-password-again"):
        drv.find_element(By.ID, fid).send_keys("correct horse battery")
    submit(drv, "create-account-form")
    text = alert_text(drv)
    _, _, _, props = ax(drv, "#new-email")
    check("an empty email address is caught", "Enter an email address" in text, text)
    check("...focus on the email field, marked invalid",
          focused_id(drv) == "new-email" and props.get("invalid") == "true", f"{focused_id(drv)} {props.get('invalid')}")

    # 2. Created: announced, banner to confirm.
    drv.find_element(By.ID, "new-email").send_keys("listener@example.com")
    submit(drv, "create-account-form")
    w.until(lambda d: shown(d, "app-section"))
    status = status_text(drv, "emailed a link")
    check("creating announces the emailed link", "emailed a link to listener@example.com" in status, status)
    role, name, _, _ = ax(drv, "#email-banner")
    check("banner is a region named Confirm your email address",
          shown(drv, "email-banner") and role == "region" and name == "Confirm your email address", f"{role} {name!r}")
    to, message = wait_mail(1)
    check("the confirmation email arrives", to == "listener@example.com" and "Confirm" in message, f"{to} {message[:80]!r}")
    verify_link = link_in(message, "verify")

    # 3. Opening the link confirms the address.
    drv.get(verify_link)
    w.until(lambda d: shown(d, "app-section"))
    status = status_text(drv, "confirmed")
    check("opening the link confirms, announced", "listener@example.com confirmed" in status, status)
    check("...the token leaves the address bar", "verify=" not in drv.current_url, drv.current_url)
    time.sleep(0.3)
    check("...the banner is gone", not shown(drv, "email-banner"))
    state = drv.find_element(By.ID, "email-state").text
    check("...the Email address part says confirmed", "listener@example.com, confirmed" in state, state)

    # 4. Forgot the password.
    sign_out(drv, w)
    role, name, _, _ = ax(drv, "#show-forgot")
    check("sign-in offers Forgot your password? as a button", role == "button" and name == "Forgot your password?", f"{role} {name!r}")
    drv.find_element(By.ID, "show-forgot").click()
    w.until(lambda d: shown(d, "forgot-section"))
    check("focus moves to the Reset your password heading", focused_id(drv) == "forgot-heading", focused_id(drv))
    role, name, _, _ = ax(drv, "#forgot-account")
    check("the account field is named", name == "Username or email address", f"{role} {name!r}")
    drv.find_element(By.ID, "forgot-account").send_keys("LISTENER@example.com")
    submit(drv, "forgot-form")
    status = status_text(drv, "If that account")
    check("asking announces the neutral answer", "If that account has a confirmed email address" in status, status)
    to, message = wait_mail(2)
    check("the reset email arrives", to == "listener@example.com" and "Reset your AudioNet password" in message, f"{to} {message[:80]!r}")
    reset_link = link_in(message, "reset")

    # 5. Choosing a new password.
    drv.get(reset_link)
    w.until(lambda d: shown(d, "reset-section"))
    check("the link opens Choose a new password, focused", focused_id(drv) == "reset-heading", focused_id(drv))
    check("...the token leaves the address bar", "reset=" not in drv.current_url, drv.current_url)
    drv.find_element(By.ID, "reset-password").send_keys("a whole new password")
    drv.find_element(By.ID, "reset-password-again").send_keys("a whole new pasword")
    submit(drv, "reset-form")
    text = alert_text(drv)
    check("different passwords are caught", "two passwords are different" in text and focused_id(drv) == "reset-password-again",
          f"{text} {focused_id(drv)}")
    field = drv.find_element(By.ID, "reset-password-again")
    field.clear()
    field.send_keys("a whole new password")
    submit(drv, "reset-form")
    w.until(lambda d: shown(d, "app-section"))
    status = status_text(drv, "Password changed")
    check("setting it signs in and announces it", "Password changed. You are signed in as listener" in status, status)
    check("...focus on the device list heading", focused_id(drv) == "devices-heading", focused_id(drv))
    sign_out(drv, w)
    drv.find_element(By.ID, "username").send_keys("listener")
    drv.find_element(By.ID, "password").send_keys("correct horse battery")
    submit(drv, "sign-in-form")
    text = alert_text(drv)
    check("the old password no longer works", "incorrect" in text, text)
    drv.find_element(By.ID, "alert")  # keep the page
    drv.find_element(By.ID, "username").clear()
    drv.find_element(By.ID, "password").clear()

    # 6. An account without an address.
    drv.find_element(By.ID, "username").send_keys("veteran")
    drv.find_element(By.ID, "password").send_keys("correct horse battery")
    submit(drv, "sign-in-form")
    w.until(lambda d: shown(d, "app-section"))
    status = status_text(drv, "no email address")
    check("signing in mentions the missing address", "no email address; adding one is highly recommended" in status, status)
    role, name, _, _ = ax(drv, "#email-banner")
    banner = drv.find_element(By.ID, "email-banner-text").text
    check("banner is a region named Add an email address", role == "region" and name == "Add an email address", f"{role} {name!r}")
    check("...saying it is highly recommended and only for password resets",
          "highly recommended" in banner and "only to reset your password" in banner, banner)

    # 7. Adding one.
    drv.find_element(By.ID, "banner-add-email").click()
    check("the banner button opens the form on the address field", focused_id(drv) == "email-address", focused_id(drv))
    _, _, _, props = ax(drv, "#change-email")
    check("...the Email address button is expanded", props.get("expanded") is True, str(props))
    drv.find_element(By.ID, "email-address").send_keys("veteran@example.com")
    drv.find_element(By.ID, "email-password").send_keys("not the password")
    submit(drv, "email-form")
    text = alert_text(drv)
    check("a wrong password is refused, focus on it", "incorrect" in text and focused_id(drv) == "email-password",
          f"{text} {focused_id(drv)}")
    drv.find_element(By.ID, "email-password").clear()
    drv.find_element(By.ID, "email-password").send_keys("correct horse battery")
    submit(drv, "email-form")
    status = status_text(drv, "Email address saved")
    check("saving announces the emailed link", "emailed a link to veteran@example.com" in status, status)
    check("...focus on the Email address heading", focused_id(drv) == "email-heading", focused_id(drv))
    to, message = wait_mail(3)
    check("the confirmation email arrives", to == "veteran@example.com", str(to))
    role, name, _, _ = ax(drv, "#email-banner")
    check("the banner now asks to confirm", name == "Confirm your email address", name)
    drv.find_element(By.ID, "banner-send-link").click()
    status = status_text(drv, "new link")
    check("Send the link again announces it", "emailed a new link" in status, status)
    to, message = wait_mail(4)
    check("...and the email arrives", to == "veteran@example.com", str(to))

    # 8. Changing a confirmed address keeps it until the new one is confirmed.
    drv.get(link_in(message, "verify"))
    w.until(lambda d: shown(d, "app-section"))
    status_text(drv, "confirmed")
    time.sleep(0.3)
    drv.find_element(By.ID, "change-email").click()
    field = drv.find_element(By.ID, "email-address")
    field.clear()
    field.send_keys("veteran.new@example.com")
    drv.find_element(By.ID, "email-password").send_keys("correct horse battery")
    submit(drv, "email-form")
    status = status_text(drv, "changes when you open it")
    check("changing announces that the old address stays until confirmed",
          "veteran.new@example.com" in status and "still go to veteran@example.com" in status, status)
    state = drv.find_element(By.ID, "email-state").text
    check("...the Email address part says both", "veteran@example.com, confirmed. Changing to veteran.new@example.com" in state, state)
    banner = drv.find_element(By.ID, "email-banner-text").text
    check("...the banner asks to confirm the new one", shown(drv, "email-banner") and "still go to veteran@example.com" in banner, banner)
    wait_mail(6)
    sent_to = sorted(t for t, _ in inbox[4:6])
    check("...a link to the new address and a notice to the old one",
          sent_to == ["veteran.new@example.com", "veteran@example.com"], str(sent_to))
finally:
    drv.quit()
    if proc:
        proc.terminate()
print(f"{sum(results)} of {len(results)} checks passed")
sys.exit(0 if results and all(results) else 1)

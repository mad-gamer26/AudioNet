// AudioNet web client.
//
// Works under any domain: every request is same-origin, so a self-hosted
// server serves this file and it talks to that server.
//
// Accessibility rules followed here:
// - Native controls only (buttons, selects, forms), all labelled.
// - State changes are announced once, politely, via #status; errors via
//   #alert. Periodic diagnostics are never announced.
// - Device updates change existing elements in place so focus and the
//   screen reader's position are not lost.
"use strict";

const PROTOCOL_VERSION = { major: 0, minor: 1 };
const SOFTWARE = "audionet-web 0.1.0";

const $ = (id) => document.getElementById(id);

const state = {
  username: null,
  // The account's confirmed email address (where password-reset links go),
  // and one waiting for confirmation (dropped after 7 days unless confirmed).
  email: null,
  pendingEmail: null,
  // Whether this server can send email (confirmation and password reset).
  passwordReset: false,
  ws: null,
  wsBackoff: 1000,
  iceServers: [],
  nodes: new Map(), // node_id -> summary
  sessions: new Map(), // session_id -> session
  connected: false,
};

// ─── announcements ──────────────────────────────────────────────────────────

// Every announcement and problem also goes to the status log, with the time.
const eventLog = [];
function logEvent(text) {
  const time = new Date().toLocaleTimeString();
  eventLog.push(`${time}  ${text}`);
  if (eventLog.length > 300) eventLog.splice(0, eventLog.length - 300);
  $("event-log").textContent = eventLog.join("\n");
}

let announceTimer = null;
function announce(text) {
  logEvent(text);
  // Clear then set, so repeating the same text is announced again.
  const el = $("status");
  el.textContent = "";
  clearTimeout(announceTimer);
  announceTimer = setTimeout(() => { el.textContent = text; }, 50);
}

function showError(text) {
  logEvent(`Problem: ${text}`);
  const el = $("alert");
  el.textContent = "";
  setTimeout(() => { el.textContent = text; }, 50);
}

function clearError() {
  $("alert").textContent = "";
}

// ─── HTTP ───────────────────────────────────────────────────────────────────

async function api(method, path, body) {
  const resp = await fetch(path, {
    method,
    credentials: "same-origin",
    headers: body ? { "Content-Type": "application/json" } : {},
    body: body ? JSON.stringify(body) : undefined,
  });
  let data = null;
  try { data = await resp.json(); } catch (_) { /* empty body */ }
  if (!resp.ok) {
    const message = data && data.error && data.error.message
      ? data.error.message
      : `The server answered with an error (HTTP ${resp.status}).`;
    const err = new Error(message);
    err.status = resp.status;
    throw err;
  }
  return data;
}

// ─── sign-in ────────────────────────────────────────────────────────────────

// The signed-out screens; exactly one is shown at a time.
const SIGNED_OUT_SECTIONS = ["sign-in-section", "create-account-section", "forgot-section", "reset-section"];

function showOnly(id) {
  $("app-section").hidden = true;
  $("email-banner").hidden = true;
  $("account").hidden = true;
  for (const s of SIGNED_OUT_SECTIONS) $(s).hidden = s !== id;
}

function showSignIn() {
  showOnly("sign-in-section");
}

// Links in emails and from the apps open this page with one of these; the
// link's token leaves the address bar (and the history) at once.
function takeLinkFromAddress() {
  const params = new URLSearchParams(location.search);
  const link = { verify: params.get("verify"), reset: params.get("reset"), forgot: params.has("forgot") };
  if (link.verify || link.reset || link.forgot) history.replaceState(null, "", location.pathname);
  return link;
}

async function start() {
  const link = takeLinkFromAddress();
  try {
    const info = await api("GET", "/api/v1/info");
    $("create-account-offer").hidden = !info.allow_registration;
    state.passwordReset = !!info.password_reset;
    $("forgot-offer").hidden = !state.passwordReset;
  } catch (_) { /* optional */ }
  if (link.reset) {
    showReset(link.reset);
    return;
  }
  if (link.verify) await verifyEmail(link.verify);
  try {
    const me = await api("GET", "/api/v1/me");
    signedIn(me, false);
  } catch (_) {
    if (link.forgot && state.passwordReset) showForgot();
    else {
      showSignIn();
      if (link.forgot) showError("This server cannot reset passwords by email. Ask its administrator to set a new password.");
    }
  }
}

// `me` is the account as /api/v1/me or a sign-in answers it.
function signedIn(me, moveFocus) {
  state.username = me.username;
  $("account-name").textContent = me.username;
  showSignInHelp(me.username);
  showOnly(null);
  $("account").hidden = false;
  $("app-section").hidden = false;
  if ("email" in me) setEmailState(me.email, me.pending_email);
  else loadEmailState();
  if (moveFocus) $("devices-heading").focus();
  connect();
}

// ─── email address ──────────────────────────────────────────────────────────

async function loadEmailState() {
  try {
    const me = await api("GET", "/api/v1/me");
    setEmailState(me.email, me.pending_email);
  } catch (_) { /* shown on the next sign-in */ }
}

// Shows the addresses in the Email address part and, when there is no
// confirmed one or a change is waiting, the banner above the devices.
function setEmailState(email, pending) {
  state.email = email || null;
  state.pendingEmail = pending || null;
  const banner = $("email-banner");
  const confirmed = state.email;
  const waiting = state.pendingEmail;
  $("change-email").textContent = confirmed || waiting ? "Change the email address" : "Add an email address";
  if (!confirmed && !waiting) {
    $("email-state").textContent = "Email address: none. Adding one is highly recommended: it is used only to reset your password if you forget it.";
    $("email-banner-heading").textContent = "Add an email address";
    $("email-banner-text").textContent = "An email address is highly recommended. It is used only to reset your password if you forget it.";
    $("banner-add-email").hidden = false;
    $("banner-send-link").hidden = true;
    banner.hidden = false;
  } else if (waiting && !state.passwordReset) {
    // This server cannot send the link: nothing to confirm, nothing expires.
    $("email-state").textContent = confirmed
      ? `Email address: ${confirmed}. Changing to ${waiting} (this server cannot send email to confirm it).`
      : `Email address: ${waiting} (this server cannot send email to confirm it).`;
    banner.hidden = true;
  } else if (waiting) {
    $("email-state").textContent = confirmed
      ? `Email address: ${confirmed}, confirmed. Changing to ${waiting}: open the link AudioNet emailed there within 7 days. Until then, password-reset links still go to ${confirmed}.`
      : `Email address: ${waiting}, not confirmed yet. Open the link AudioNet emailed there within 7 days; until then it cannot be used to reset your password, and after that it is removed.`;
    $("email-banner-heading").textContent = "Confirm your email address";
    $("email-banner-text").textContent = confirmed
      ? `AudioNet emailed a link to ${waiting}. Open it within 7 days to switch to that address; until then, password-reset links still go to ${confirmed}. Check the spam folder if it did not arrive.`
      : `AudioNet emailed a link to ${waiting}. Open it within 7 days to confirm the address, so it can be used to reset your password if you forget it; unconfirmed addresses are removed after 7 days. Check the spam folder if it did not arrive.`;
    $("banner-add-email").hidden = true;
    $("banner-send-link").hidden = false;
    banner.hidden = false;
  } else {
    $("email-state").textContent = `Email address: ${confirmed}, confirmed. It is used only to reset your password if you forget it.`;
    banner.hidden = true;
  }
}

function openEmailForm() {
  clearError();
  $("email-form").hidden = false;
  $("change-email").setAttribute("aria-expanded", "true");
  $("email-address").value = state.pendingEmail || state.email || "";
  $("email-address").focus();
}

function closeEmailForm(focusButton) {
  $("email-form").hidden = true;
  $("change-email").setAttribute("aria-expanded", "false");
  $("email-password").value = "";
  for (const id of ["email-address", "email-password"]) markInvalid(id, false);
  if (focusButton) $("change-email").focus();
}

$("change-email").addEventListener("click", () => {
  if ($("email-form").hidden) openEmailForm();
  else closeEmailForm(true);
});
$("banner-add-email").addEventListener("click", openEmailForm);
$("email-cancel").addEventListener("click", () => closeEmailForm(true));

$("email-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  clearError();
  const email = $("email-address").value.trim();
  const password = $("email-password").value;
  for (const id of ["email-address", "email-password"]) markInvalid(id, false);
  let problem = null;
  if (!looksLikeEmail(email)) problem = ["email-address", "Enter an email address like name@example.com."];
  else if (!password) problem = ["email-password", "Enter your password."];
  if (problem) {
    markInvalid(problem[0], true);
    showError(problem[1]);
    $(problem[0]).focus();
    return;
  }
  try {
    const r = await api("POST", "/api/v1/account/email", { email, password });
    closeEmailForm(false);
    setEmailState(r.email, r.pending_email);
    if (!r.pending_email) announce(`Email address: ${r.email}, as before.`);
    else if (!r.link_sent) announce(`Email address saved: ${r.pending_email}.`);
    else if (r.email) announce(`AudioNet emailed a link to ${r.pending_email}. The address changes when you open it; until then, password-reset links still go to ${r.email}, which was told about the change.`);
    else announce(`Email address saved. AudioNet emailed a link to ${r.pending_email}; open it within 7 days to confirm the address.`);
    $("email-heading").focus();
  } catch (err) {
    showError(err.message);
    const field = /password/i.test(err.message) ? "email-password" : "email-address";
    markInvalid(field, true);
    $(field).focus();
  }
});

$("banner-send-link").addEventListener("click", async () => {
  clearError();
  try {
    const r = await api("POST", "/api/v1/account/email/send-link");
    setEmailState(r.email, r.pending_email);
    if (!r.pending_email) {
      announce("The email address is already confirmed.");
      $("email-heading").focus();
    } else {
      announce(`AudioNet emailed a new link to ${r.pending_email}; it works for 7 days. Earlier links no longer work.`);
    }
  } catch (err) {
    showError(err.message);
  }
});

// The same rule as the server's, to catch slips without a round trip.
function looksLikeEmail(email) {
  return /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email) && !/\.$/.test(email) && email.length <= 254;
}

async function verifyEmail(token) {
  try {
    const r = await api("POST", "/api/v1/email/verify", { token });
    announce(`Email address ${r.email} confirmed for ${r.username}. It can now be used to reset the password.`);
  } catch (err) {
    showError(`The email address was not confirmed. ${err.message} Signed in, choose Send the link again.`);
  }
}

// ─── forgotten password ─────────────────────────────────────────────────────

function showForgot() {
  clearError();
  showOnly("forgot-section");
  $("forgot-form").hidden = false;
  $("forgot-sent").hidden = true;
  if (!$("forgot-account").value) $("forgot-account").value = $("username").value.trim();
  $("forgot-heading").focus();
}

$("show-forgot").addEventListener("click", showForgot);

for (const id of ["forgot-back", "reset-back"]) {
  $(id).addEventListener("click", () => {
    clearError();
    showSignIn();
    $("sign-in-heading").focus();
  });
}

$("forgot-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  clearError();
  const account = $("forgot-account").value.trim();
  markInvalid("forgot-account", false);
  if (!account) {
    markInvalid("forgot-account", true);
    showError("Enter your username or email address.");
    $("forgot-account").focus();
    return;
  }
  try {
    await api("POST", "/api/v1/password/forgot", { account });
    const text = "If that account has a confirmed email address, AudioNet has emailed it a link to choose a new password. The link works for 1 hour. Check the spam folder if it does not arrive.";
    $("forgot-sent").textContent = text;
    $("forgot-sent").hidden = false;
    announce(text);
  } catch (err) {
    showError(err.message);
    $("forgot-account").focus();
  }
});

let resetToken = null;

function showReset(token) {
  resetToken = token;
  clearError();
  showOnly("reset-section");
  $("reset-heading").focus();
}

$("reset-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  clearError();
  const password = $("reset-password").value;
  const again = $("reset-password-again").value;
  for (const id of ["reset-password", "reset-password-again"]) markInvalid(id, false);
  let problem = null;
  if ([...password].length < 10) problem = ["reset-password", "The password must be at least 10 characters long."];
  else if (password !== again) problem = ["reset-password-again", "The two passwords are different. Type the same password in both fields."];
  if (problem) {
    markInvalid(problem[0], true);
    showError(problem[1]);
    $(problem[0]).focus();
    return;
  }
  try {
    const r = await api("POST", "/api/v1/password/reset", { token: resetToken, password });
    resetToken = null;
    $("reset-password").value = "";
    $("reset-password-again").value = "";
    announce(`Password changed. You are signed in as ${r.username}; other browsers were signed out.`);
    signedIn(r, true);
  } catch (err) {
    showError(err.message);
    if (/link/i.test(err.message)) {
      // The link is used up or expired: offer a new one.
      showForgot();
    } else {
      markInvalid("reset-password", true);
      $("reset-password").focus();
    }
  }
});

async function submitCredentials(path) {
  clearError();
  const username = $("username").value.trim();
  const password = $("password").value;
  if (!username || !password) {
    showError("Enter a username and a password.");
    return;
  }
  try {
    const r = await api("POST", path, { username, password });
    $("password").value = "";
    let me = { username: r.username };
    try { me = await api("GET", "/api/v1/me"); } catch (_) { /* loaded again below */ }
    announce(me.email === null && !me.pending_email
      ? `Signed in as ${r.username}. This account has no email address; adding one is highly recommended.`
      : `Signed in as ${r.username}.`);
    signedIn(me, true);
  } catch (e) {
    showError(e.message);
    $("password").focus();
  }
}

$("sign-in-form").addEventListener("submit", (e) => {
  e.preventDefault();
  submitCredentials("/api/v1/login");
});

// ─── creating an account ────────────────────────────────────────────────────

function markInvalid(id, invalid) {
  if (invalid) $(id).setAttribute("aria-invalid", "true");
  else $(id).removeAttribute("aria-invalid");
}

function showCreateAccount() {
  clearError();
  showOnly("create-account-section");
  // Carry over a name already typed on the sign-in form.
  if (!$("new-username").value) $("new-username").value = $("username").value.trim();
  $("create-account-heading").focus();
}

$("show-create-account").addEventListener("click", showCreateAccount);

$("back-to-sign-in").addEventListener("click", () => {
  clearError();
  showSignIn();
  $("sign-in-heading").focus();
});

$("create-account-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  clearError();
  const username = $("new-username").value.trim();
  const email = $("new-email").value.trim();
  const password = $("new-password").value;
  const again = $("new-password-again").value;
  for (const id of ["new-username", "new-email", "new-password", "new-password-again"]) markInvalid(id, false);
  // The server checks everything again; these catch the common slips
  // without a round trip.
  let problem = null;
  if (!username) problem = ["new-username", "Enter a username."];
  else if (!/^[A-Za-z0-9._-]{1,64}$/.test(username)) {
    problem = ["new-username", "A username may contain only letters, digits, dots, dashes and underscores, up to 64 characters."];
  } else if (!email) problem = ["new-email", "Enter an email address. It is used only to reset your password if you forget it."];
  else if (!looksLikeEmail(email)) problem = ["new-email", "Enter an email address like name@example.com."];
  else if ([...password].length < 10) problem = ["new-password", "The password must be at least 10 characters long."];
  else if (password !== again) problem = ["new-password-again", "The two passwords are different. Type the same password in both fields."];
  if (problem) {
    markInvalid(problem[0], true);
    showError(problem[1]);
    $(problem[0]).focus();
    return;
  }
  try {
    const r = await api("POST", "/api/v1/register", { username, password, email });
    $("new-password").value = "";
    $("new-password-again").value = "";
    announce(state.passwordReset
      ? `Account ${r.username} created. You are signed in. AudioNet emailed a link to ${email}; open it to confirm the address.`
      : `Account ${r.username} created. You are signed in.`);
    signedIn({ username: r.username, email: null, pending_email: email }, true);
  } catch (err) {
    showError(err.message);
    // Send focus where the fix is: the email address, the name when it is
    // taken or not allowed, else the password.
    const field = /email/i.test(err.message) ? "new-email"
      : /username|reserved/i.test(err.message) && !/password/i.test(err.message)
        ? "new-username" : "new-password";
    markInvalid(field, true);
    $(field).focus();
  }
});

$("sign-out").addEventListener("click", async () => {
  for (const s of [...state.sessions.values()]) stopSession(s, "You signed out.", true);
  if (state.ws) { state.ws.onclose = null; state.ws.close(); state.ws = null; }
  try { await api("POST", "/api/v1/logout"); } catch (_) { /* already signed out */ }
  state.nodes.clear();
  state.email = null;
  closeEmailForm(false);
  $("devices").textContent = "";
  showSignIn();
  announce("Signed out.");
  $("sign-in-heading").focus();
});

// ─── signaling ──────────────────────────────────────────────────────────────

function send(msg) {
  if (state.ws && state.ws.readyState === WebSocket.OPEN) {
    state.ws.send(JSON.stringify(msg));
    return true;
  }
  return false;
}

function setConnectionText(text) {
  $("connection-state").textContent = text;
}

function connect() {
  const proto = location.protocol === "https:" ? "wss:" : "ws:";
  const ws = new WebSocket(`${proto}//${location.host}/api/v1/ws`);
  state.ws = ws;
  setConnectionText("Connecting to the server…");
  ws.onopen = () => {
    ws.send(JSON.stringify({
      type: "hello",
      protocol_version: PROTOCOL_VERSION,
      client: { kind: "browser", software: SOFTWARE, platform: "browser" },
    }));
  };
  ws.onmessage = (ev) => {
    let msg;
    try { msg = JSON.parse(ev.data); } catch (_) { return; }
    handleServer(msg);
  };
  ws.onclose = () => {
    const was = state.connected;
    state.connected = false;
    for (const s of [...state.sessions.values()]) {
      stopSession(s, "The connection to the server was lost.", false);
    }
    const delay = state.wsBackoff;
    state.wsBackoff = Math.min(state.wsBackoff * 2, 30000);
    setConnectionText(`Not connected to the server. Retrying in ${Math.round(delay / 1000)} seconds.`);
    if (was) announce("The connection to the server was lost. Reconnecting.");
    setTimeout(() => { if (state.username) connect(); }, delay);
  };
}

function handleServer(msg) {
  switch (msg.type) {
    case "welcome":
      state.connected = true;
      state.wsBackoff = 1000;
      state.iceServers = msg.ice_servers.map((s) => {
        const o = { urls: s.urls };
        if (s.username) o.username = s.username;
        if (s.credential) o.credential = s.credential;
        return o;
      });
      setConnectionText("Connected to the server.");
      send({ type: "list_nodes" });
      break;
    case "nodes":
      state.nodes.clear();
      for (const n of msg.nodes) state.nodes.set(n.node_id, n);
      renderDevices();
      break;
    case "node_update": {
      const before = state.nodes.get(msg.node.node_id);
      state.nodes.set(msg.node.node_id, msg.node);
      if (before && before.online !== msg.node.online) {
        announce(`${msg.node.name} is now ${msg.node.online ? "online" : "offline"}.`);
      } else if (before && msg.node.online && sharing(before) !== sharing(msg.node)) {
        announce(`${msg.node.name} ${sharing(msg.node) ? "started" : "stopped"} sharing its audio.`);
      }
      renderDevices();
      break;
    }
    case "session_answer": {
      const s = state.sessions.get(msg.session_id);
      if (s) s.pc.setRemoteDescription({ type: "answer", sdp: msg.sdp }).catch((e) => {
        stopSession(s, `The device's answer could not be used: ${e.message}`, true);
      });
      break;
    }
    case "session_status": {
      const s = state.sessions.get(msg.session_id);
      if (!s) break;
      if (msg.detail) setSessionText(s, msg.detail);
      if (msg.state === "failed") {
        stopSession(s, msg.detail || "The device reported a failure.", false);
        break;
      }
      // Warnings from the device (for example a microphone sending only
      // silence) are announced once, and so is the all-clear after one.
      if (msg.detail && msg.detail.startsWith("Warning:")) {
        s.warned = true;
        announce(`${s.title}: ${msg.detail}`);
        break;
      }
      if (s.warned && msg.state === "active" && msg.detail) {
        s.warned = false;
        announce(`${s.title}: ${msg.detail}`);
        break;
      }
      // After the stream first connects (announced separately), announce
      // each change between interrupted and flowing once, e.g. an audio
      // device removed and plugged back in.
      if (msg.state === "active" && !s.deviceActive) {
        if (s.deviceWasActive && msg.detail) announce(`${s.title}: ${msg.detail}`);
        s.deviceActive = s.deviceWasActive = true;
      } else if (msg.state === "starting" && s.deviceActive) {
        s.deviceActive = false;
        if (msg.detail) announce(`${s.title}: ${msg.detail}`);
      }
      break;
    }
    case "session_end": {
      const s = state.sessions.get(msg.session_id);
      if (s) stopSession(s, msg.reason, false);
      break;
    }
    case "error": {
      // A refused offer names its session: end it with the reason.
      const s = msg.session_id && state.sessions.get(msg.session_id);
      if (s) stopSession(s, msg.message, false);
      else showError(msg.message);
      break;
    }
    default:
      break;
  }
}

// ─── devices ────────────────────────────────────────────────────────────────

function platformName(p) {
  return { windows: "Windows", mac_os: "macOS", linux: "Linux", ios: "iPhone or iPad", android: "Android", browser: "browser" }[p] || "unknown system";
}

function setOptions(select, items, placeholder) {
  // Update options in place, keeping the current selection when possible.
  const current = select.value;
  const key = JSON.stringify(items.map((i) => [i.id, i.label]));
  if (select.dataset.key === key) return;
  select.dataset.key = key;
  select.textContent = "";
  if (items.length === 0) {
    const o = document.createElement("option");
    o.value = "";
    o.textContent = placeholder;
    select.appendChild(o);
    select.disabled = true;
    return;
  }
  select.disabled = false;
  for (const i of items) {
    const o = document.createElement("option");
    o.value = i.id;
    o.textContent = i.label;
    select.appendChild(o);
  }
  if (items.some((i) => i.id === current)) select.value = current;
  else {
    const d = items.find((i) => i.isDefault);
    if (d) select.value = d.id;
  }
}

function deviceElement(node) {
  const id = `device-${node.node_id}`;
  let el = document.getElementById(id);
  if (el) return el;
  // A native disclosure: one line (name and state) until expanded, so a
  // long device list stays short. Screen readers announce it as collapsed
  // or expanded; Enter or Space toggles it. The element is reused on every
  // refresh, so what is expanded (and focus) stays put.
  el = document.createElement("details");
  el.className = "device";
  el.id = id;
  el.innerHTML = `
    <summary><span id="${id}-name" class="name"></span><span class="summary"></span></summary>
    <div class="device-body">
    <form class="listen">
      <div class="row">
        <p><label for="${id}-source">Sound to listen to</label>
        <select id="${id}-source"></select></p>
        <p><button type="submit">Listen</button></p>
      </div>
    </form>
    <form class="speak">
      <div class="row">
        <p><label for="${id}-dest">Play my microphone on</label>
        <select id="${id}-dest"></select></p>
        <p><label><input type="checkbox" class="voice" checked> Voice processing (echo cancellation and noise suppression)</label></p>
        <p><button type="submit">Send my microphone</button></p>
      </div>
    </form>
    <p><button type="button" class="remove">Remove this device from my account</button></p>
    </div>`;
  el.querySelector(".listen").addEventListener("submit", (e) => {
    e.preventDefault();
    const sel = el.querySelector(`#${CSS.escape(id)}-source`);
    if (sel.value) startListen(node.node_id, sel.value, sel.selectedOptions[0].textContent);
  });
  el.querySelector(".speak").addEventListener("submit", (e) => {
    e.preventDefault();
    const sel = el.querySelector(`#${CSS.escape(id)}-dest`);
    const voice = el.querySelector(".voice").checked;
    if (sel.value) startSpeak(node.node_id, sel.value, sel.selectedOptions[0].textContent, voice);
  });
  el.querySelector(".remove").addEventListener("click", async () => {
    const n = state.nodes.get(node.node_id);
    if (!confirm(`Remove ${n ? n.name : "this device"} from your account? It will need to sign in again.`)) return;
    try {
      await api("DELETE", `/api/v1/nodes/${encodeURIComponent(node.node_id)}`);
      state.nodes.delete(node.node_id);
      el.remove();
      announce(`${n ? n.name : "The device"} was removed.`);
      $("devices-heading").focus();
      renderDevices();
    } catch (e) {
      showError(e.message);
    }
  });
  $("devices").appendChild(el);
  return el;
}

/// Whether a device shares its audio (servers before sharing did not say:
/// online meant sharing). Not sharing, it can still be sent to.
function sharing(n) {
  return n.online && n.sharing !== false;
}

function renderDevices() {
  const nodes = [...state.nodes.values()].sort((a, b) =>
    (b.online - a.online) || a.name.localeCompare(b.name));
  $("no-devices").hidden = nodes.length > 0;
  // Remove elements for devices that no longer exist.
  for (const el of [...$("devices").children]) {
    if (!state.nodes.has(el.id.replace(/^device-/, ""))) el.remove();
  }
  for (const n of nodes) {
    const el = deviceElement(n);
    const id = el.id;
    const name = el.querySelector(`#${CSS.escape(id)}-name`);
    if (name.textContent !== n.name) name.textContent = n.name;
    const summary = !n.online
      ? `: offline, ${platformName(n.platform)}. Start AudioNet on it to use it.`
      : sharing(n)
        ? `: online, ${platformName(n.platform)}, ${n.sources.length} sounds, ${n.destinations.length} outputs`
        : `: online, not sharing its audio, ${platformName(n.platform)}, ${n.destinations.length} outputs`;
    const p = el.querySelector(".summary");
    if (p.textContent !== summary) p.textContent = summary;
    p.className = `summary ${n.online ? "state-online" : "state-offline"}`;
    const sources = n.sources.map((s) => ({
      id: s.id,
      label: `${s.name}${s.source_type === "input" ? " (input)" : ""}${s.is_default ? " (default)" : ""}`,
      isDefault: s.is_default && s.source_type === "loopback",
    }));
    const dests = n.destinations.map((d) => ({ id: d.id, label: `${d.name}${d.is_default ? " (default)" : ""}`, isDefault: d.is_default }));
    // A device that does not share can be sent to, not listened to.
    setOptions(el.querySelector(`#${CSS.escape(id)}-source`), sharing(n) ? sources : [],
      !n.online ? "Device is offline" : sharing(n) ? "No sources available" : "Not sharing its audio");
    setOptions(el.querySelector(`#${CSS.escape(id)}-dest`), dests, n.online ? "No outputs available" : "Device is offline");
    el.querySelector(".listen button").disabled = !sharing(n) || !state.connected;
    el.querySelector(".speak button").disabled = !n.online || !state.connected;
  }
}

// ─── sessions ───────────────────────────────────────────────────────────────

function newSessionId() {
  return (crypto.randomUUID ? crypto.randomUUID() : String(Math.random()).slice(2));
}

// Ask for stereo Opus: browsers decode stereo only when their own SDP says so.
function preferStereo(sdp) {
  const m = sdp.match(/a=rtpmap:(\d+) opus\/48000\/2/i);
  if (!m) return sdp;
  const pt = m[1];
  return sdp.replace(new RegExp(`a=fmtp:${pt} ([^\\r\\n]*)`), (line, params) =>
    /stereo=1/.test(params) ? line : `a=fmtp:${pt} ${params};stereo=1;sprop-stereo=1`);
}

function waitForIceGathering(pc, ms) {
  if (pc.iceGatheringState === "complete") return Promise.resolve();
  return new Promise((resolve) => {
    const done = () => { pc.removeEventListener("icegatheringstatechange", check); resolve(); };
    const check = () => { if (pc.iceGatheringState === "complete") done(); };
    pc.addEventListener("icegatheringstatechange", check);
    setTimeout(done, ms);
  });
}

function sessionElement(s) {
  const li = document.createElement("li");
  li.innerHTML = `<p class="title"></p><p class="state"></p>
    <p class="stream-volume">
      <label class="mute-label"><input type="checkbox" class="mute"> Mute</label>
      <label class="volume-label">Volume <input type="range" class="volume" min="0" max="100" step="5" value="100"></label>
      <span class="volume-value" aria-hidden="true">100%</span>
    </p>
    <p><button type="button" class="stop"></button></p>`;
  li.querySelector(".title").textContent = s.title;
  li.querySelector(".stop").textContent = `Stop: ${s.title}`;
  li.querySelector(".stop").addEventListener("click", () => stopSession(s, "You stopped it.", true));
  // Per-stream volume and mute, named after the stream for screen readers.
  const mute = li.querySelector(".mute");
  const volume = li.querySelector(".volume");
  mute.setAttribute("aria-label", `Mute ${s.title}`);
  volume.setAttribute("aria-label", `Volume for ${s.title}`);
  const update = () => {
    s.volume = Number(volume.value) / 100;
    s.muted = mute.checked;
    volume.setAttribute("aria-valuetext", `${volume.value} percent${s.muted ? ", muted" : ""}`);
    li.querySelector(".volume-value").textContent = `${volume.value}%${s.muted ? ", muted" : ""}`;
    applyVolume(s);
  };
  mute.addEventListener("change", update);
  volume.addEventListener("input", update);
  update();
  $("streams").appendChild(li);
  $("no-streams").hidden = true;
  return li;
}

// A stream's volume slider (0 to 1) is heard on a square curve, as in the
// apps: half-way is about 12 dB quieter. Muted is silence.
function streamGain(s) {
  return s.muted ? 0 : s.volume * s.volume;
}

// Listening: the audio element plays at the stream's volume. Sending: the
// microphone goes through a gain node before it is sent (a short ramp, so
// a change never clicks).
function applyVolume(s) {
  if (s.audio) {
    s.audio.volume = s.volume * s.volume;
    s.audio.muted = s.muted;
  }
  if (s.gain) s.gain.gain.setTargetAtTime(streamGain(s), s.gain.context.currentTime, 0.003);
}

function setSessionText(s, text) {
  s.stateText = text;
  s.el.querySelector(".state").textContent = text;
}

async function openSession({ nodeId, media, title, localStream }) {
  clearError();
  const node = state.nodes.get(nodeId);
  const s = {
    id: newSessionId(),
    nodeId,
    media,
    title: `${title} on ${node ? node.name : "device"}`,
    pc: new RTCPeerConnection({
      iceServers: state.iceServers,
      // Troubleshooting: add ?ice=relay to the address to force all audio
      // through the TURN relay (tests firewall and relay setup).
      iceTransportPolicy: new URLSearchParams(location.search).get("ice") === "relay" ? "relay" : "all",
    }),
    localStream,
    audio: null,
    volume: 1,
    muted: false,
    gain: null,
    audioContext: null,
    stats: "",
    announcedActive: false,
  };
  state.sessions.set(s.id, s);
  s.el = sessionElement(s);
  setSessionText(s, "Starting…");
  announce(`Starting: ${s.title}.`);

  if (media.kind === "listen") {
    s.pc.addTransceiver("audio", { direction: "recvonly" });
    s.pc.ontrack = (ev) => {
      const audio = new Audio();
      audio.autoplay = true;
      audio.srcObject = ev.streams[0] || new MediaStream([ev.track]);
      s.audio = audio;
      applyVolume(s);
      audio.play().catch(() => showError("The browser blocked playback. Press the Listen button again."));
    };
  } else {
    // The microphone through the stream's gain (volume and mute), in stereo.
    const ctx = new AudioContext({ latencyHint: "interactive" });
    const source = ctx.createMediaStreamSource(localStream);
    const gain = ctx.createGain();
    const out = ctx.createMediaStreamDestination();
    out.channelCount = 2;
    source.connect(gain).connect(out);
    s.audioContext = ctx;
    s.gain = gain;
    s.sent = out.stream;
    applyVolume(s);
    for (const track of out.stream.getAudioTracks()) s.pc.addTrack(track, out.stream);
  }
  s.pc.onconnectionstatechange = () => {
    const c = s.pc.connectionState;
    if (c === "connected" && !s.announcedActive) {
      s.announcedActive = true;
      setSessionText(s, "Connected. Audio is flowing.");
      announce(`${s.title}: connected.`);
    } else if (c === "failed") {
      stopSession(s, "The audio connection failed. A firewall or network may be blocking it.", true);
    }
  };

  try {
    const offer = await s.pc.createOffer();
    offer.sdp = preferStereo(offer.sdp);
    await s.pc.setLocalDescription(offer);
    await waitForIceGathering(s.pc, 3000);
    const ok = send({
      type: "session_offer",
      session_id: s.id,
      node_id: nodeId,
      media,
      sdp: s.pc.localDescription.sdp,
    });
    if (!ok) stopSession(s, "Not connected to the server.", false);
  } catch (e) {
    stopSession(s, `Could not start: ${e.message}`, false);
  }
}

function startListen(nodeId, sourceId, label) {
  openSession({ nodeId, media: { kind: "listen", source_id: sourceId }, title: `Listening to ${label}` });
}

async function startSpeak(nodeId, destinationId, label, voice) {
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: voice, noiseSuppression: voice, autoGainControl: voice, channelCount: 2 },
    });
  } catch (e) {
    showError(`The microphone could not be opened: ${e.message}. Check the browser's microphone permission.`);
    return;
  }
  openSession({
    nodeId,
    media: { kind: "speak", destination_id: destinationId },
    title: `Sending your microphone to ${label}`,
    localStream: stream,
  });
}

function stopSession(s, reason, notifyServer) {
  if (!state.sessions.has(s.id)) return;
  state.sessions.delete(s.id);
  if (notifyServer) send({ type: "session_end", session_id: s.id, reason: "Stopped in the web client." });
  try { s.pc.close(); } catch (_) { /* closed */ }
  if (s.audio) { s.audio.srcObject = null; }
  if (s.localStream) for (const t of s.localStream.getTracks()) t.stop();
  if (s.audioContext) s.audioContext.close().catch(() => {});
  const hadFocus = s.el.contains(document.activeElement);
  s.el.remove();
  $("no-streams").hidden = state.sessions.size > 0;
  announce(`Stopped: ${s.title}. ${reason}`);
  if (hadFocus) $("streams-heading").setAttribute("tabindex", "-1"), $("streams-heading").focus();
  updateDiagnostics();
}

// ─── diagnostics ────────────────────────────────────────────────────────────

function fmt(n, digits = 1) {
  return Number.isFinite(n) ? n.toFixed(digits) : "not measured";
}

async function sessionDiagnostics(s) {
  const lines = [`${s.title}`, `  State: ${s.stateText || "unknown"}; connection ${s.pc.connectionState}`];
  let stats;
  try { stats = await s.pc.getStats(); } catch (_) { return lines.join("\n"); }
  let pair = null;
  let fallbackPair = null;
  const byId = new Map();
  stats.forEach((r) => byId.set(r.id, r));
  stats.forEach((r) => {
    // The pair in use is the transport's selected pair; several pairs can
    // be nominated and succeeded at once (for example the relay first, then
    // a direct path found later).
    if (r.type === "transport" && r.selectedCandidatePairId) pair = byId.get(r.selectedCandidatePairId) || pair;
    if (r.type === "candidate-pair" && r.nominated && r.state === "succeeded") fallbackPair = r;
    if (r.type === "inbound-rtp" && r.kind === "audio") {
      const received = r.totalSamplesReceived || 0;
      const concealed = r.concealedSamples || 0;
      const jb = r.jitterBufferEmittedCount ? (r.jitterBufferDelay / r.jitterBufferEmittedCount) * 1000 : NaN;
      lines.push(`  Packets received: ${r.packetsReceived}; lost: ${r.packetsLost}`);
      lines.push(`  Network jitter: ${fmt((r.jitter || 0) * 1000)} ms; jitter buffer delay: ${fmt(jb)} ms`);
      lines.push(`  Concealed audio: ${fmt(received ? (concealed / received) * 100 : 0, 2)} percent of samples`);
      if (typeof r.audioLevel === "number") lines.push(`  Audio level: ${fmt(r.audioLevel > 0 ? 20 * Math.log10(r.audioLevel) : -Infinity)} dBFS`);
    }
    if (r.type === "outbound-rtp" && r.kind === "audio") {
      lines.push(`  Packets sent: ${r.packetsSent}; kilobytes sent: ${Math.round((r.bytesSent || 0) / 1000)}`);
    }
  });
  pair = pair || fallbackPair;
  if (pair) {
    const local = byId.get(pair.localCandidateId);
    const remote = byId.get(pair.remoteCandidateId);
    lines.push(`  Round-trip time: ${fmt((pair.currentRoundTripTime || 0) * 1000)} ms`);
    const through = local && remote && (local.candidateType === "relay" || remote.candidateType === "relay")
      ? " (through the TURN relay)"
      : local && remote && local.candidateType === "host" && remote.candidateType === "host" ? " (direct, on this network)" : " (direct)";
    lines.push(`  Route: local ${local ? local.candidateType : "?"}, remote ${remote ? remote.candidateType : "?"}${through}`);
  }
  return lines.join("\n");
}

async function updateDiagnostics() {
  const parts = [];
  for (const s of state.sessions.values()) parts.push(await sessionDiagnostics(s));
  $("diagnostics").textContent = parts.length ? parts.join("\n\n") : "No active streams.";
}
setInterval(updateDiagnostics, 2000);

// ─── status log dialog ──────────────────────────────────────────────────────

$("open-status-log").addEventListener("click", () => {
  const dialog = $("status-log");
  dialog.showModal();
  // Start reading at the newest event.
  const events = $("event-log");
  events.scrollTop = events.scrollHeight;
  events.focus();
});
$("close-status-log").addEventListener("click", () => $("status-log").close());
$("status-log").addEventListener("close", () => $("open-status-log").focus());

$("copy-status-log").addEventListener("click", async () => {
  const text = `Events
${$("event-log").textContent}

Diagnostics
${$("diagnostics").textContent}`;
  try {
    await navigator.clipboard.writeText(text);
    announce("Status log copied to the clipboard.");
  } catch (_) {
    showError("The browser did not allow copying. Select the status log text and copy it manually.");
  }
});

// ─── adding a device ────────────────────────────────────────────────────────

function showSignInHelp(username) {
  $("server-address").textContent = location.origin;
  $("sign-in-command").textContent =
    `audionet node sign-in --server ${location.origin} --user ${username || "YOUR-NAME"}`;
}

$("copy-sign-in").addEventListener("click", async () => {
  try {
    await navigator.clipboard.writeText($("sign-in-command").textContent);
    announce("Command copied.");
  } catch (_) {
    showError("Copying failed. Select the command text and copy it manually.");
  }
});

start();

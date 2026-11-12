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
  // This computer's microphones ({ id, label }), for "Send my microphone".
  microphones: [],
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
  const link = {
    verify: params.get("verify"), reset: params.get("reset"), forgot: params.has("forgot"),
    // A one-time sign-in link from the server's administrator.
    signin: params.get("signin"),
    // The apps' "Manage devices": opens that tab once signed in.
    manage: params.has("manage"),
  };
  if (link.verify || link.reset || link.forgot || link.manage || link.signin) history.replaceState(null, "", location.pathname);
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
  state.openManage = link.manage;
  let linkError = null;
  if (link.signin) {
    try {
      const r = await api("POST", "/api/v1/login/link", { token: link.signin });
      announce(`Signed in as ${r.username} with a sign-in link.`);
    } catch (err) {
      linkError = `The sign-in link did not work. ${err.message}`;
    }
  }
  try {
    const me = await api("GET", "/api/v1/me");
    signedIn(me, false);
    if (linkError) showError(linkError);
  } catch (_) {
    if (linkError) {
      showSignIn();
      showError(linkError);
      return;
    }
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
  loadManagedDevices();
  if (state.openManage) {
    // From an app's "Manage devices" (also after signing in first).
    state.openManage = false;
    showTab("manage", true);
  } else {
    showTab("devices", moveFocus);
  }
  connect();
  refreshMicrophones();
}

// ─── tabs ───────────────────────────────────────────────────────────────────

const TABS = ["devices", "streams", "manage", "account"];
const TAB_HEADINGS = { devices: "devices-heading", streams: "streams-heading", manage: "manage-heading", account: "email-heading" };

// Shows one tab's panel; with `focusHeading`, focus goes to its heading
// (the tab keeps focus otherwise, as the tabs pattern expects).
function showTab(name, focusHeading) {
  for (const t of TABS) {
    const tab = $(`tab-${t}`);
    const on = t === name;
    tab.setAttribute("aria-selected", on ? "true" : "false");
    tab.tabIndex = on ? 0 : -1;
    $(`panel-${t}`).hidden = !on;
  }
  if (focusHeading) $(TAB_HEADINGS[name]).focus();
}

function currentTab() {
  return TABS.find((t) => $(`tab-${t}`).getAttribute("aria-selected") === "true") || "devices";
}

for (const t of TABS) {
  $(`tab-${t}`).addEventListener("click", () => showTab(t, false));
  $(`tab-${t}`).addEventListener("keydown", (e) => {
    const i = TABS.indexOf(t);
    const next = { ArrowRight: i + 1, ArrowLeft: i - 1, Home: 0, End: TABS.length - 1 }[e.key];
    if (next === undefined) return;
    e.preventDefault();
    const to = TABS[(next + TABS.length) % TABS.length];
    showTab(to, false);
    $(`tab-${to}`).focus();
  });
}

// The Streams tab says how many streams are running.
function updateStreamsTab() {
  const n = state.sessions.size;
  $("tab-streams").textContent = n ? `Streams (${n})` : "Streams";
}

// ─── managing devices ───────────────────────────────────────────────────────

// Every device signed in to the account (visible or not), to rename or
// remove: /api/v1/nodes lists them without any status.
async function loadManagedDevices() {
  try {
    const r = await api("GET", "/api/v1/nodes");
    renderManagedDevices(r.nodes || []);
  } catch (_) { /* tried again on the next change */ }
}

function renderManagedDevices(nodes) {
  const list = $("managed-devices");
  list.replaceChildren();
  $("no-managed-devices").hidden = nodes.length > 0;
  for (const n of [...nodes].sort((a, b) => a.name.localeCompare(b.name))) {
    const li = document.createElement("li");
    const text = document.createElement("span");
    text.textContent = `${n.name} (${platformName(n.platform)}) `;
    const rename = document.createElement("button");
    rename.type = "button";
    rename.textContent = "Rename";
    rename.setAttribute("aria-label", `Rename ${n.name}`);
    const remove = document.createElement("button");
    remove.type = "button";
    remove.textContent = "Remove";
    remove.setAttribute("aria-label", `Remove ${n.name}`);
    li.append(text, rename, " ", remove);
    rename.addEventListener("click", () => showRenameForm(li, n, rename));
    remove.addEventListener("click", () => removeDevice(n, li));
    list.appendChild(li);
  }
}

function showRenameForm(li, n, button) {
  if (li.querySelector("form")) return;
  const form = document.createElement("form");
  const id = `rename-${n.node_id}`;
  form.innerHTML = `<p><label for="${id}"></label> <input id="${id}" maxlength="64" required>
    <button type="submit">Save</button> <button type="button" class="cancel">Cancel</button></p>`;
  form.querySelector("label").textContent = `New name for ${n.name}`;
  const input = form.querySelector("input");
  input.value = n.name;
  li.appendChild(form);
  button.setAttribute("aria-expanded", "true");
  input.focus();
  input.select();
  const close = () => {
    form.remove();
    button.setAttribute("aria-expanded", "false");
    button.focus();
  };
  form.querySelector(".cancel").addEventListener("click", close);
  form.addEventListener("keydown", (e) => { if (e.key === "Escape") close(); });
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    const name = input.value.trim();
    if (!name) { input.focus(); return; }
    try {
      await api("PATCH", `/api/v1/nodes/${encodeURIComponent(n.node_id)}`, { name });
      announce(`${n.name} is now called ${name}.`);
      await loadManagedDevices();
      $("manage-heading").focus();
    } catch (err) {
      showError(err.message);
      input.focus();
    }
  });
}

async function removeDevice(n, li) {
  if (!confirm(`Remove ${n.name} from your account? It will need to sign in again.`)) return;
  try {
    await api("DELETE", `/api/v1/nodes/${encodeURIComponent(n.node_id)}`);
    state.nodes.delete(n.node_id);
    renderDevices();
    li.remove();
    announce(`${n.name} was removed from your account.`);
    await loadManagedDevices();
    $("manage-heading").focus();
  } catch (err) {
    showError(err.message);
  }
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
  showTab("account", false);
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
      showTab("account", true);
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

// The server's ICE servers as RTCPeerConnection takes them (no null fields).
function rtcIceServers(servers) {
  return (servers || []).map((s) => {
    const o = { urls: s.urls };
    if (s.username) o.username = s.username;
    if (s.credential) o.credential = s.credential;
    return o;
  });
}

function handleServer(msg) {
  switch (msg.type) {
    case "welcome":
      state.connected = true;
      state.wsBackoff = 1000;
      state.iceServers = rtcIceServers(msg.ice_servers);
      setConnectionText("Connected to the server.");
      send({ type: "list_nodes" });
      break;
    case "ice_servers":
      // Fresh TURN credentials: the Welcome's expire, and a tab left open
      // longer would start every session without a relay. New sessions use
      // these.
      state.iceServers = rtcIceServers(msg.ice_servers);
      break;
    case "nodes":
      // Only visible devices are listed (servers before 1.3 also listed
      // devices that were not connected).
      state.nodes.clear();
      for (const n of msg.nodes) if (n.online) state.nodes.set(n.node_id, n);
      renderDevices();
      break;
    case "node_update": {
      // A device became visible or changed; `online: false` says it is no
      // longer visible (visibility off, or disconnected).
      const before = state.nodes.get(msg.node.node_id);
      if (msg.node.online) {
        state.nodes.set(msg.node.node_id, msg.node);
        if (!before) announce(`${msg.node.name} is now visible.`);
      } else if (before) {
        state.nodes.delete(msg.node.node_id);
        announce(`${msg.node.name} is no longer visible.`);
      }
      renderDevices();
      // A new device or a rename shows in Manage devices too.
      if (msg.node.online && (!before || before.name !== msg.node.name)) loadManagedDevices();
      break;
    }
    case "session_answer": {
      const s = state.sessions.get(msg.session_id);
      // Sending in stereo: the device decodes stereo, but its answer does
      // not say so, and browsers send stereo only when the answer does.
      const sendStereo = s && s.stereo && s.media.kind === "speak";
      if (s) s.pc.setRemoteDescription({ type: "answer", sdp: sendStereo ? preferStereo(msg.sdp) : msg.sdp }).catch((e) => {
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
        <p><label for="${id}-mic">My microphone</label>
        <select id="${id}-mic" class="mic"></select></p>
        <p><label><input type="checkbox" class="voice"> Voice processing (echo cancellation and noise suppression; sends mono instead of stereo)</label></p>
        <p><button type="submit">Send my microphone</button></p>
      </div>
    </form>
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
    const mic = el.querySelector(".mic");
    if (sel.value) startSpeak(node.node_id, sel.value, sel.selectedOptions[0].textContent, voice, mic.value, mic.selectedOptions[0]?.textContent || "");
  });
  el.querySelector(".mic").addEventListener("change", (e) => saveMicrophoneChoice(e.target.value));
  $("devices").appendChild(el);
  return el;
}

// The visible devices (only visible devices are in state.nodes).
function renderDevices() {
  const nodes = [...state.nodes.values()].sort((a, b) => a.name.localeCompare(b.name));
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
    const count = (k, one, many) => `${k} ${k === 1 ? one : many}`;
    const summary = `: ${platformName(n.platform)}, ${count(n.sources.length, "sound", "sounds")}, ${count(n.destinations.length, "output", "outputs")}`;
    const p = el.querySelector(".summary");
    if (p.textContent !== summary) p.textContent = summary;
    const sources = n.sources.map((s) => ({
      id: s.id,
      label: `${s.name}${s.source_type === "input" ? " (input)" : ""}${s.is_default ? " (default)" : ""}`,
      isDefault: s.is_default && s.source_type === "loopback",
    }));
    const dests = n.destinations.map((d) => ({ id: d.id, label: `${d.name}${d.is_default ? " (default)" : ""}`, isDefault: d.is_default }));
    setOptions(el.querySelector(`#${CSS.escape(id)}-source`), sources, "No sources available");
    setOptions(el.querySelector(`#${CSS.escape(id)}-dest`), dests, "No outputs available");
    const mic = el.querySelector(".mic");
    setOptions(mic, microphoneOptions(), "No microphone found");
    // The microphone chosen last time, once the browser lists it.
    const saved = savedMicrophone();
    if (saved && mic.value === "" && state.microphones.some((m) => m.id === saved)) mic.value = saved;
    el.querySelector(".listen button").disabled = !state.connected;
    el.querySelector(".speak button").disabled = !state.connected;
  }
}

// ─── microphones ────────────────────────────────────────────────────────────

// The microphone last chosen is kept in this browser and chosen again next
// time. "" is the browser's default microphone.
const MIC_KEY = "audionet-microphone";

function savedMicrophone() {
  try { return localStorage.getItem(MIC_KEY) || ""; } catch (_) { return ""; }
}

function saveMicrophoneChoice(id) {
  try { localStorage.setItem(MIC_KEY, id); } catch (_) { /* not kept */ }
}

// The choices for "My microphone": the default, then each microphone.
// Browsers show the names only once the microphone has been allowed for
// this site; until then they are numbered.
function microphoneOptions() {
  const saved = savedMicrophone();
  const known = state.microphones.some((m) => m.id === saved);
  return [
    { id: "", label: "Default microphone", isDefault: !known },
    ...state.microphones.map((m) => ({ id: m.id, label: m.label, isDefault: m.id === saved })),
  ];
}

async function refreshMicrophones() {
  if (!navigator.mediaDevices || !navigator.mediaDevices.enumerateDevices) return;
  let devices;
  try { devices = await navigator.mediaDevices.enumerateDevices(); } catch (_) { return; }
  // Chrome also lists "default" and "communications" entries that stand for
  // another microphone: the default option covers them.
  const inputs = devices.filter((d) => d.kind === "audioinput" && d.deviceId &&
    d.deviceId !== "default" && d.deviceId !== "communications");
  state.microphones = inputs.map((d, i) => ({ id: d.deviceId, label: d.label || `Microphone ${i + 1}` }));
  renderDevices();
}

if (navigator.mediaDevices && navigator.mediaDevices.addEventListener) {
  navigator.mediaDevices.addEventListener("devicechange", refreshMicrophones);
}

// ─── sessions ───────────────────────────────────────────────────────────────

function newSessionId() {
  return (crypto.randomUUID ? crypto.randomUUID() : String(Math.random()).slice(2));
}

// Stereo Opus in an SDP's Opus format line. In this browser's own offer it
// asks to receive stereo (browsers decode stereo only when their own SDP
// says so); in the device's answer it lets this browser send stereo
// (browsers encode stereo only when the other side's SDP says so).
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

async function openSession({ nodeId, media, title, localStream, stereo }) {
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
  updateStreamsTab();
  s.el = sessionElement(s);
  setSessionText(s, "Starting…");
  announce(`Starting: ${s.title}.`);

  s.stereo = media.kind === "listen" || Boolean(stereo);
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
    // The microphone through the stream's gain (volume and mute): stereo
    // without voice processing, mono with it (echo cancellation works on
    // one channel).
    const ctx = new AudioContext({ latencyHint: "interactive" });
    const source = ctx.createMediaStreamSource(localStream);
    const gain = ctx.createGain();
    const out = ctx.createMediaStreamDestination();
    out.channelCount = s.stereo ? 2 : 1;
    out.channelCountMode = "explicit";
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
    if (s.stereo) offer.sdp = preferStereo(offer.sdp);
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

// Sends this computer's microphone (`micId`, or the default for "") to a
// device's output. Voice processing off: stereo, as the microphone records
// it; on: mono, with echo cancellation, noise suppression and automatic
// gain.
async function startSpeak(nodeId, destinationId, label, voice, micId, micLabel) {
  const audio = {
    echoCancellation: voice,
    noiseSuppression: voice,
    autoGainControl: voice,
    channelCount: voice ? 1 : { ideal: 2 },
  };
  if (micId) audio.deviceId = { exact: micId };
  // The microphone chosen last time, while the browser does not list it yet
  // (it lists microphones only once allowed): preferred, not required.
  else if (savedMicrophone()) audio.deviceId = { ideal: savedMicrophone() };
  let stream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ audio });
  } catch (e) {
    if (micId && (e.name === "OverconstrainedError" || e.name === "NotFoundError")) {
      showError(`The microphone ${micLabel} is not available. Choose another one under My microphone.`);
    } else {
      showError(`The microphone could not be opened: ${e.message}. Check the browser's microphone permission.`);
    }
    return;
  }
  // Allowed now: the browser tells the microphones' names.
  refreshMicrophones();
  const settings = stream.getAudioTracks()[0]?.getSettings() || {};
  const channels = voice ? "mono" : settings.channelCount === 1 ? "stereo (this microphone records one channel)" : "stereo";
  const name = micId ? micLabel : settings.deviceId && state.microphones.find((m) => m.id === settings.deviceId)?.label;
  openSession({
    nodeId,
    media: { kind: "speak", destination_id: destinationId },
    title: `Sending your microphone to ${label}`,
    localStream: stream,
    stereo: !voice,
  });
  logEvent(`Microphone: ${name || "default microphone"}, ${channels}, voice processing ${voice ? "on" : "off"}.`);
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
  updateStreamsTab();
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

// Measurements are for troubleshooting: shown (and gathered) only when
// "Show measurements" is checked in the status log. The choice is kept in
// this browser.
function measurementsOn() {
  return $("show-measurements").checked;
}

function applyMeasurementsChoice() {
  $("measurements").hidden = !measurementsOn();
  if (measurementsOn()) updateDiagnostics();
}

try {
  $("show-measurements").checked = localStorage.getItem("audionet-show-measurements") === "1";
} catch (_) { /* storage unavailable: off */ }
applyMeasurementsChoice();
$("show-measurements").addEventListener("change", () => {
  try {
    localStorage.setItem("audionet-show-measurements", measurementsOn() ? "1" : "0");
  } catch (_) { /* not kept; still applies now */ }
  applyMeasurementsChoice();
});

async function updateDiagnostics() {
  if (!measurementsOn()) return;
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
  let text = `Events
${$("event-log").textContent}`;
  if (measurementsOn()) {
    text += `

Measurements
${$("diagnostics").textContent}`;
  }
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

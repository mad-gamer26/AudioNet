"use strict";

// ─── basics ─────────────────────────────────────────────────────────────────

const $ = (id) => document.getElementById(id);
const state = { timer: null };

function h(tag, attrs, ...kids) {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs || {})) {
    if (v === null || v === undefined || v === false) continue;
    if (k === "class") el.className = v;
    else if (k === "text") el.textContent = v;
    else if (k.startsWith("on")) el.addEventListener(k.slice(2), v);
    else if (v === true) el.setAttribute(k, "");
    else el.setAttribute(k, v);
  }
  for (const kid of kids.flat()) {
    if (kid === null || kid === undefined || kid === false) continue;
    el.append(kid instanceof Node ? kid : document.createTextNode(String(kid)));
  }
  return el;
}

let uid = 0;
const nextId = (p) => `${p}-${++uid}`;
const plural = (n, one, many) => `${n} ${n === 1 ? one : many || one + "s"}`;

function announce(text) {
  const el = $("announce");
  el.textContent = "";
  setTimeout(() => { el.textContent = text; }, 50);
}

function showError(text) {
  const el = $("alert");
  el.textContent = "";
  setTimeout(() => { el.textContent = text; }, 50);
}

function clearError() { $("alert").textContent = ""; }

async function api(method, path, body) {
  const opts = { method, headers: { "X-AudioNet-Admin": "1" } };
  if (body !== undefined) {
    opts.headers["Content-Type"] = "application/json";
    opts.body = JSON.stringify(body);
  }
  let resp;
  try { resp = await fetch(path, opts); } catch (_) {
    throw new Error("The page could not reach the AudioNet server.");
  }
  let data = {};
  try { data = await resp.json(); } catch (_) { /* empty */ }
  if (!resp.ok) throw new Error((data.error && data.error.message) || `The server answered with an error (HTTP ${resp.status}).`);
  return data;
}

// Times: the date and time here, and how long ago.
function when(secs) {
  if (!secs) return "Never";
  const d = new Date(secs * 1000);
  return d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "short" });
}

function ago(secs) {
  if (!secs) return "never";
  const s = Math.max(0, Math.round(Date.now() / 1000 - secs));
  if (s < 60) return "just now";
  if (s < 3600) return `${plural(Math.round(s / 60), "minute")} ago`;
  if (s < 86400) return `${plural(Math.round(s / 3600), "hour")} ago`;
  return `${plural(Math.round(s / 86400), "day")} ago`;
}

function duration(secs) {
  const s = Math.max(0, Math.round(Date.now() / 1000 - secs));
  if (s < 60) return plural(s, "second");
  if (s < 3600) return plural(Math.floor(s / 60), "minute");
  const hrs = Math.floor(s / 3600);
  if (hrs < 48) return `${plural(hrs, "hour")} ${plural(Math.floor((s % 3600) / 60), "minute")}`;
  return plural(Math.floor(hrs / 24), "day");
}

const PLATFORMS = { windows: "Windows", mac_os: "Mac", macos: "Mac", linux: "Linux", ios: "iPhone", android: "Android" };
const platformText = (p) => PLATFORMS[p] || p || "Unknown";

function setView(title, current) {
  $("page-heading").textContent = title;
  document.title = title === "AudioNet admin" ? title : `${title} - AudioNet admin`;
  for (const a of $("tabs").querySelectorAll("a")) {
    if (a.getAttribute("href") === current) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  }
  const view = $("view");
  view.textContent = "";
  return view;
}

function field(label, input, help) {
  const id = input.id || (input.id = nextId("f"));
  const p = h("p", { class: "field" }, h("label", { for: id, text: label }), " ", input);
  if (help) {
    const hid = nextId("help");
    input.setAttribute("aria-describedby", hid);
    p.append(h("span", { id: hid, class: "help", text: help }));
  }
  return p;
}

function secret(label, help) {
  const input = h("input", { type: "password", autocomplete: "new-password", spellcheck: "false" });
  input.id = nextId("pw");
  const btn = h("button", { type: "button", "aria-pressed": "false", "aria-controls": input.id, "aria-label": `Show ${label.toLowerCase()}` }, "Show");
  btn.addEventListener("click", () => {
    const show = input.type === "password";
    input.type = show ? "text" : "password";
    btn.textContent = show ? "Hide" : "Show";
    btn.setAttribute("aria-pressed", String(show));
    btn.setAttribute("aria-label", `${show ? "Hide" : "Show"} ${label.toLowerCase()}`);
  });
  const p = field(label, input, help);
  p.append(" ", btn);
  return { el: p, input };
}

function table(caption, heads, rows) {
  return h("table", {},
    h("caption", { class: "visually-hidden", text: caption }),
    h("thead", {}, h("tr", {}, heads.map((t) => h("th", { scope: "col", text: t })))),
    h("tbody", {}, rows));
}

// Keeps focus on the same control when a list redraws.
function keepFocus(render) {
  const key = document.activeElement && document.activeElement.dataset ? document.activeElement.dataset.key : null;
  render();
  if (key) {
    const again = document.querySelector(`[data-key="${CSS.escape(key)}"]`);
    if (again) again.focus();
  }
}

// Copying: the modern clipboard where the browser offers it, else the older
// copy command (this page is often plain http).
async function copyText(text) {
  if (window.isSecureContext && navigator.clipboard) {
    try { await navigator.clipboard.writeText(text); return true; } catch (_) { /* fall back */ }
  }
  const back = document.activeElement;
  const ta = h("textarea", { readonly: true, "aria-hidden": "true", class: "offscreen" });
  ta.value = text;
  document.body.append(ta);
  ta.select();
  let ok = false;
  try { ok = document.execCommand("copy"); } catch (_) { ok = false; }
  ta.remove();
  if (back) back.focus();
  return ok;
}

async function act(path, body, then) {
  clearError();
  try {
    const r = await api("POST", path, body || {});
    if (then) await then(r);
    announce(r.message || "Done.");
    return r;
  } catch (e) {
    showError(e.message);
    return null;
  }
}

function flash(message, focusKey) {
  sessionStorage.setItem("admin-flash", message || "");
  sessionStorage.setItem("admin-focus", focusKey || "");
}

// ─── overview ───────────────────────────────────────────────────────────────

async function viewOverview() {
  const view = setView("AudioNet admin", "#/");
  const d = await api("GET", "/api/overview");
  const c = d.counts;
  view.append(
    h("p", { text: `AudioNet server ${d.version}, running for ${duration(d.started)}.` }),
    h("h2", { text: "Now" }),
    h("ul", {},
      h("li", { text: `${plural(c.connections, "connection")}: ${plural(c.devices_connected, "device")} and ${plural(c.connections - c.devices_connected, "browser")}.` }),
      h("li", { text: `${plural(c.sessions, "stream")} running.` }),
      h("li", { text: `${plural(c.users, "account")}, ${plural(c.devices, "device")}, ${plural(c.browser_sessions, "browser sign-in")}.` }),
      h("li", { text: d.email ? "Email is on: addresses can be confirmed and passwords reset." : "Email is off: no address confirmation or password reset." })),
    h("h2", { text: "Settings" }),
    h("p", { class: "note", text: "From the configuration file; change them there and restart the server. Secrets are not shown." }),
    table("Server settings", ["Setting", "Value"], d.settings.map(([k, v]) => h("tr", {},
      h("th", { scope: "row", text: k }),
      h("td", { text: v === null || v === undefined || v === "" ? "Not set" : v === true ? "Yes" : v === false ? "No" : String(v) })))));
}

// ─── accounts ───────────────────────────────────────────────────────────────

async function viewAccounts() {
  const view = setView("Accounts", "#/accounts");
  const out = h("div", { class: "result" });
  const listWrap = h("div");
  const username = h("input", { type: "text", autocomplete: "off", spellcheck: "false", maxlength: 32 });
  const pw = secret("Password", "At least 10 characters.");
  const email = h("input", { type: "email", autocomplete: "off", spellcheck: "false" });
  const form = h("form", { novalidate: true },
    h("h2", { id: "new-h", tabindex: "-1", text: "Create an account" }),
    field("Username", username), pw.el,
    field("Email address (optional)", email, "Recorded as confirmed: it can reset the password."),
    h("p", {}, h("button", { type: "submit" }, "Create the account")), out);
  form.addEventListener("submit", async (e) => {
    e.preventDefault();
    if (!username.value.trim()) { showError("Give the account a username."); username.focus(); return; }
    if (pw.input.value.length < 10) { showError("The password must be at least 10 characters long."); pw.input.focus(); return; }
    const r = await act("/api/users", { username: username.value.trim(), password: pw.input.value, email: email.value.trim() || null });
    if (r) {
      flash(r.message, `user:${username.value.trim()}`);
      route();
    }
  });
  view.append(h("section", { "aria-labelledby": "list-h" }, h("h2", { id: "list-h", text: "All accounts" }), listWrap), form);
  const d = await api("GET", "/api/users");
  listWrap.append(h("p", { text: `${plural(d.users.length, "account")}, sorted by name.` }),
    table("Accounts, sorted by name", ["Username", "Email", "Devices", "Connected now", "Last device seen", "Browser sign-ins", "Created"],
      d.users.map(({ user: u, connected }) => h("tr", {},
        h("th", { scope: "row" }, h("a", { href: `#/account/${encodeURIComponent(u.username)}`, "data-key": `user:${u.username}` }, u.username)),
        h("td", { text: u.email || (u.pending_email ? `None (waiting: ${u.pending_email})` : "None") }),
        h("td", { text: String(u.devices) }),
        h("td", { text: connected ? plural(connected, "connection") : "No" }),
        h("td", { text: u.last_seen ? ago(u.last_seen) : "Never" }),
        h("td", { text: String(u.web_sessions) }),
        h("td", { text: when(u.created_at) })))));
}

async function viewAccount(name) {
  const view = setView(`Account ${name}`, "#/accounts");
  let d;
  try { d = await api("GET", `/api/users/${encodeURIComponent(name)}`); } catch (e) {
    view.append(h("p", {}, e.message, " ", h("a", { href: "#/accounts" }, "Back to the accounts")));
    return;
  }
  const u = d.user;
  const base = `/api/users/${encodeURIComponent(name)}`;
  const reload = (message, key) => { flash(message, key); route(); };
  view.append(h("p", {}, h("a", { href: "#/accounts" }, "All accounts")),
    h("dl", {},
      h("dt", { text: "Email" }), h("dd", { text: u.email || "None" }),
      u.pending_email ? [h("dt", { text: "Waiting for confirmation" }), h("dd", { text: u.pending_email })] : null,
      h("dt", { text: "Created" }), h("dd", { text: when(u.created_at) }),
      h("dt", { text: "Devices" }), h("dd", { text: String(d.devices.length) }),
      h("dt", { text: "Browser sign-ins" }), h("dd", { text: String(d.web_sessions.length) }),
      h("dt", { text: "Unused sign-in links" }), h("dd", { text: String(d.sign_in_links) })));

  // Devices.
  const devRows = d.devices.map(({ device: dev, connection: c }) => {
    const status = c
      ? `Connected for ${duration(c.since)} from ${c.address || "an unknown address"} with ${c.software || "an unknown app"}. ${c.visible ? "Visible to the account." : "Not visible."}`
      : `Not connected. Last seen ${ago(dev.last_seen)}.`;
    return h("tr", {},
      h("th", { scope: "row", text: dev.name }),
      h("td", { text: platformText(dev.platform) }),
      h("td", { text: status }),
      h("td", { text: dev.push ? "On" : "Off" }),
      h("td", { text: when(dev.created_at) }),
      h("td", {},
        h("button", { type: "button", "aria-label": `Rename ${dev.name}`, "data-key": `dev:rename:${dev.id}`, onclick: () => {
          const next = prompt(`New name for ${dev.name}:`, dev.name);
          if (next === null || !next.trim() || next.trim() === dev.name) return;
          act(`/api/devices/${encodeURIComponent(dev.id)}/rename`, { name: next.trim() }, (r) => reload(r.message, `dev:rename:${dev.id}`));
        } }, "Rename"),
        c ? h("button", { type: "button", "aria-label": `Disconnect ${dev.name}`, "data-key": `dev:disc:${dev.id}`, onclick: () => {
          if (!confirm(`Disconnect ${dev.name}? It stays signed in and reconnects by itself.`)) return;
          act(`/api/connections/${encodeURIComponent(c.conn_id)}/disconnect`, {}, (r) => reload(r.message, `dev:rename:${dev.id}`));
        } }, "Disconnect") : null,
        h("button", { type: "button", class: "danger", "aria-label": `Remove ${dev.name}`, "data-key": `dev:del:${dev.id}`, onclick: () => {
          if (!confirm(`Remove ${dev.name} from ${name}'s account? It is signed out and disconnected at once, and needs the password to sign in again.`)) return;
          act(`/api/devices/${encodeURIComponent(dev.id)}/delete`, {}, (r) => reload(r.message, "devices-h"));
        } }, "Remove")));
  });
  view.append(h("section", { "aria-labelledby": "devices-h" },
    h("h2", { id: "devices-h", tabindex: "-1", "data-key": "devices-h", text: `Devices (${d.devices.length})` }),
    devRows.length ? table(`${name}'s devices`, ["Name", "System", "Status", "Push notifications", "Signed in", "Actions"], devRows)
      : h("p", { text: "No devices are signed in to this account." })));

  // Browsers connected now.
  view.append(h("section", { "aria-labelledby": "br-h" },
    h("h2", { id: "br-h", text: `Browsers connected now (${d.browsers.length})` }),
    d.browsers.length ? table(`${name}'s open browsers`, ["Connected", "From", "App", "Actions"], d.browsers.map((c) => h("tr", {},
      h("th", { scope: "row", text: `${when(c.since)} (${duration(c.since)})` }),
      h("td", { text: c.address || "Unknown" }),
      h("td", { text: c.software || "Unknown" }),
      h("td", {}, h("button", { type: "button", "data-key": `br:${c.conn_id}`, "aria-label": `Disconnect the browser connected since ${when(c.since)}`, onclick: () => {
        act(`/api/connections/${encodeURIComponent(c.conn_id)}/disconnect`, {}, (r) => reload(r.message, "br-h"));
      } }, "Disconnect")))))
      : h("p", { text: "None." })));

  // Browser sign-ins.
  view.append(h("section", { "aria-labelledby": "si-h" },
    h("h2", { id: "si-h", tabindex: "-1", "data-key": "si-h", text: `Browser sign-ins (${d.web_sessions.length})` }),
    d.web_sessions.length ? [
      table(`${name}'s browser sign-ins`, ["Signed in", "Lasts until", "Actions"], d.web_sessions.map((w) => h("tr", {},
        h("th", { scope: "row", text: when(w.created_at) }),
        h("td", { text: when(w.expires_at) }),
        h("td", {}, h("button", { type: "button", "aria-label": `End the sign-in from ${when(w.created_at)}`, "data-key": `si:${w.id}`, onclick: () => {
          if (!confirm(`End the browser sign-in from ${when(w.created_at)}?`)) return;
          act(`${base}/sessions/${w.id}/delete`, {}, (r) => reload(r.message, "si-h"));
        } }, "Sign out"))))),
      h("p", {}, h("button", { type: "button", onclick: () => {
        if (!confirm(`Sign out every browser of ${name}? Unused sign-in links are cancelled too. Devices stay signed in.`)) return;
        act(`${base}/sign-out-browsers`, {}, (r) => reload(r.message, "si-h"));
      } }, "Sign out every browser")),
    ] : h("p", { text: "None." })));

  // Sign-in link.
  const hours = h("select", {}, [1, 2, 6, 24, 48, 72, 168].map((n) => new Option(plural(n, "hour"), n)));
  hours.value = "24";
  const linkOut = h("div", { class: "result" });
  view.append(h("section", { "aria-labelledby": "link-h" },
    h("h2", { id: "link-h", text: "One-time sign-in link" }),
    h("p", { text: "Signs one web browser in to this account without the password, once. It works in the web client only; apps and devices still need the password." }),
    field("Works for", hours),
    h("p", {}, h("button", { type: "button", onclick: async () => {
      const r = await act(`${base}/link`, { hours: Number(hours.value) });
      if (!r) return;
      linkOut.textContent = "";
      const input = h("input", { type: "text", readonly: true, class: "wide", value: r.link });
      const copied = await copyText(r.link);
      linkOut.append(field("Sign-in link", input, copied ? "Copied to the clipboard." : "The browser didn't allow copying: the link is selected, press Control+C (Command+C on a Mac)."));
      announce(`${r.message} ${copied ? "Copied to the clipboard." : "Select and copy it."}`);
      if (!copied) { input.focus(); input.select(); }
    } }, "Make a sign-in link and copy it")), linkOut));

  // Password and email.
  const pw = secret("New password", "At least 10 characters. Browsers are signed out; devices stay signed in.");
  const pwForm = h("form", { novalidate: true }, h("h2", { text: "Password" }), pw.el, h("p", {}, h("button", { type: "submit" }, "Set the password")));
  pwForm.addEventListener("submit", (e) => {
    e.preventDefault();
    if (pw.input.value.length < 10) { showError("The password must be at least 10 characters long."); pw.input.focus(); return; }
    act(`${base}/password`, { password: pw.input.value }, (r) => { pw.input.value = ""; announce(r.message); });
  });
  const em = h("input", { type: "email", value: u.email || "", autocomplete: "off", spellcheck: "false" });
  const emForm = h("form", { novalidate: true }, h("h2", { text: "Email address" }),
    field("Email address", em, "Recorded as confirmed. Leave empty and save to remove it."),
    h("p", {}, h("button", { type: "submit" }, "Save the email address")));
  emForm.addEventListener("submit", (e) => {
    e.preventDefault();
    act(`${base}/email`, { email: em.value.trim() || null }, (r) => reload(r.message, ""));
  });
  view.append(pwForm, emForm);

  // Delete.
  view.append(h("section", { "aria-labelledby": "del-h" }, h("h2", { id: "del-h", text: "Delete the account" }),
    h("p", { text: "Deletes the account, its devices and its sign-ins, and disconnects everything of it at once. It can't be undone." }),
    h("p", {}, h("button", { type: "button", class: "danger", onclick: async () => {
      if (!confirm(`Delete the account ${name} and all its devices?`)) return;
      if (!confirm(`Really delete ${name}? This can't be undone.`)) return;
      const r = await act(`${base}/delete`, {});
      if (r) { flash(r.message, ""); location.hash = "#/accounts"; }
    } }, `Delete ${name}`))));
}

// ─── live ───────────────────────────────────────────────────────────────────

async function viewLive() {
  const view = setView("Live", "#/live");
  const wrap = h("div");
  view.append(h("p", { class: "note", text: "Updates every 5 seconds." }), wrap);
  let last = "";
  const render = (d) => {
    wrap.textContent = "";
    const byConn = Object.fromEntries(d.connections.map((c) => [c.conn_id, c]));
    const who = (c) => (c ? (c.node_name ? `${c.username}'s ${c.node_name}` : `a browser of ${c.username}`) : "a closed connection");
    wrap.append(h("section", { "aria-labelledby": "conn-h" },
      h("h2", { id: "conn-h", text: `Connections (${d.connections.length})` }),
      d.connections.length ? table("Connections, by account", ["Account", "Device", "From", "App", "Connected", "Shares", "Actions"],
        d.connections.map((c) => h("tr", {},
          h("th", { scope: "row" }, h("a", { href: `#/account/${encodeURIComponent(c.username)}` }, c.username)),
          h("td", { text: c.node_name ? `${c.node_name} (${platformText(c.platform)}${c.visible ? ", visible" : ", not visible"})` : "Web browser" }),
          h("td", { text: c.address || "Unknown" }),
          h("td", { text: c.software || "Unknown" }),
          h("td", { text: duration(c.since) }),
          h("td", { text: c.node_name ? `${plural(c.sources.length, "source")}${c.sources.length ? ": " + c.sources.join(", ") : ""}. ${plural(c.destinations.length, "output")}${c.destinations.length ? ": " + c.destinations.join(", ") : ""}.` : "Nothing" }),
          h("td", {}, h("button", { type: "button", "data-key": `live:${c.conn_id}`, "aria-label": `Disconnect ${who(c)}`, onclick: () => {
            if (!confirm(`Disconnect ${who(c)}? It stays signed in and reconnects by itself.`)) return;
            act(`/api/connections/${encodeURIComponent(c.conn_id)}/disconnect`, {}, () => refresh());
          } }, "Disconnect")))))
        : h("p", { text: "Nothing is connected." })),
    h("section", { "aria-labelledby": "str-h" },
      h("h2", { id: "str-h", text: `Streams (${d.sessions.length})` }),
      d.sessions.length ? table("Streams, oldest first", ["Who", "What", "State", "For"], d.sessions.map((s) => {
        const from = byConn[s.from_conn];
        const to = byConn[s.to_conn];
        const what = s.direction === "listen"
          ? `Listening to ${who(to)}: ${s.endpoint}`
          : `Sending to ${who(to)}: ${s.endpoint}`;
        return h("tr", {}, h("th", { scope: "row", text: who(from) }), h("td", { text: what }),
          h("td", { text: { starting: "Starting", active: "Playing", ended: "Ended", failed: "Failed" }[s.state] || s.state }),
          h("td", { text: duration(s.started) }));
      })) : h("p", { text: "No streams are running." })));
  };
  const refresh = async () => {
    try {
      const d = await api("GET", "/api/live");
      const json = JSON.stringify(d);
      if (json !== last) { last = json; keepFocus(() => render(d)); }
    } catch (e) { showError(e.message); }
  };
  await refresh();
  state.timer = setInterval(refresh, 5000);
}

// ─── log ────────────────────────────────────────────────────────────────────

async function viewLog() {
  const view = setView("Log", "#/log");
  const lines = h("select", {}, [100, 300, 1000, 2000].map((n) => new Option(`Last ${n} lines`, n)));
  lines.value = "300";
  const filter = h("input", { type: "search", spellcheck: "false" });
  const adminOnly = h("input", { type: "checkbox", id: nextId("c") });
  const out = h("div");
  let all = [];
  const show = () => {
    const f = filter.value.trim().toLowerCase();
    const shown = all.filter((l) => (!adminOnly.checked || l.includes(" admin: ")) && (!f || l.toLowerCase().includes(f)));
    out.textContent = "";
    const id = nextId("logl");
    out.append(h("p", { id, text: `${plural(shown.length, "line")}, newest last.` }),
      shown.length ? h("pre", { tabindex: "0", "aria-labelledby": id, text: shown.join("\n") }) : null);
    return shown.length;
  };
  const load = async (say) => {
    clearError();
    try {
      all = (await api("GET", `/api/log?lines=${lines.value}`)).lines;
      const n = show();
      if (say) announce(`${plural(n, "line")} shown.`);
    } catch (e) { showError(e.message); }
  };
  filter.addEventListener("input", show);
  adminOnly.addEventListener("change", () => announce(`${plural(show(), "line")} shown.`));
  view.append(h("p", { class: "note", text: "The server's recent log lines, kept in memory (the last 2000). The full log is in the system journal." }),
    field("Show", lines), field("Only lines containing", filter),
    h("p", { class: "check" }, adminOnly, " ", h("label", { for: adminOnly.id, text: "Only admin actions" })),
    h("p", {}, h("button", { type: "button", onclick: () => load(true) }, "Refresh")), out);
  lines.addEventListener("change", () => load(true));
  await load(false);
}

// ─── routing ────────────────────────────────────────────────────────────────

async function route() {
  clearInterval(state.timer);
  const parts = (location.hash || "#/").slice(2).split("/").map(decodeURIComponent);
  const flashMsg = sessionStorage.getItem("admin-flash");
  const focusKey = sessionStorage.getItem("admin-focus");
  sessionStorage.removeItem("admin-flash");
  sessionStorage.removeItem("admin-focus");
  if (flashMsg === null) clearError();
  try {
    if (parts[0] === "accounts") await viewAccounts();
    else if (parts[0] === "account" && parts[1]) await viewAccount(parts[1]);
    else if (parts[0] === "live") await viewLive();
    else if (parts[0] === "log") await viewLog();
    else await viewOverview();
  } catch (e) { showError(e.message); }
  if (flashMsg) announce(flashMsg);
  const target = focusKey && document.querySelector(`[data-key="${CSS.escape(focusKey)}"]`);
  (target || $("page-heading")).focus();
}

window.addEventListener("hashchange", route);
route();

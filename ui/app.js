"use strict";

const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

let state = null;

const $ = (id) => document.getElementById(id);
const el = (sel, root = document) => root.querySelector(sel);

// --- boot --------------------------------------------------------------------
async function init() {
  bindNav();
  bindControls();
  await listen("state", (e) => render(e.payload));
  await listen("first-success", (e) => showToast(`First upload to ${e.payload} ✓`));
  await listen("navigate", (e) => onNavigate(e.payload));
  setupDrop();
  try {
    render(await invoke("get_state"));
  } catch (err) {
    console.error("get_state failed", err);
  }
}

// --- navigation --------------------------------------------------------------
function bindNav() {
  document.querySelectorAll(".nav-btn").forEach((btn) => {
    btn.addEventListener("click", () => gotoPage(btn.dataset.page));
  });
}

function gotoPage(page) {
  document.querySelectorAll(".nav-btn").forEach((b) =>
    b.classList.toggle("active", b.dataset.page === page)
  );
  document.querySelectorAll(".page").forEach((p) =>
    p.classList.toggle("active", p.dataset.page === page)
  );
}

function onNavigate(target) {
  if (target === "settings") gotoPage("settings");
  else if (target === "add_host") {
    gotoPage("hosts");
    openAddHost();
  }
}

// --- controls ----------------------------------------------------------------
function bindControls() {
  $("power").addEventListener("click", () => invoke("toggle"));
  $("host-card").addEventListener("click", openHostPicker);
  $("recent-all").addEventListener("click", openRecentPanel);
  $("add-host-btn").addEventListener("click", openAddHost);
  $("reload-hosts-btn").addEventListener("click", () => invoke("reload_hosts"));
  $("open-config-btn").addEventListener("click", () => invoke("open_ssh_config"));
  $("test-btn").addEventListener("click", () => invoke("test_connection"));
  $("set-test-host").addEventListener("change", (e) =>
    invoke("select_host", { host: e.target.value })
  );

  ["set-remote-dir", "set-filename", "set-screenshot-folder"].forEach((id) =>
    $(id).addEventListener("change", saveTextSettings)
  );

  $("sw-screenshots").addEventListener("click", () =>
    invoke("save_settings", {
      patch: { monitor_screenshots: !state?.settings.monitor_screenshots },
    })
  );
  $("sw-autocopy").addEventListener("click", () =>
    invoke("save_settings", {
      patch: { auto_copy_path: !state?.settings.auto_copy_path },
    })
  );
  $("sw-login").addEventListener("click", async () => {
    const on = !state?.settings.launch_at_login;
    try {
      await invoke("set_launch_at_login", { on });
    } catch (err) {
      showToast(String(err));
    }
  });

  document.querySelectorAll(".feedback-card").forEach((card) =>
    card.addEventListener("click", () => invoke("open_url", { url: card.dataset.url }))
  );

  $("panel-close").addEventListener("click", closePanel);
  $("scrim").addEventListener("click", closePanel);
}

function saveTextSettings() {
  invoke("save_settings", {
    patch: {
      remote_dir: $("set-remote-dir").value,
      filename_pattern: $("set-filename").value,
      screenshot_folder: $("set-screenshot-folder").value,
    },
  });
}

// --- render ------------------------------------------------------------------
function render(s) {
  state = s;
  document.body.dataset.phase = s.phase;
  $("version-tag").textContent = "v" + s.version;

  // power button
  const connecting = s.phase === "connecting";
  $("power-glyph").style.display = connecting ? "none" : "block";
  $("power-spinner").style.display = connecting ? "block" : "none";

  // status block
  const titles = { off: "Paste mode is off", connecting: "Connecting…", on: "Paste mode is on" };
  $("status-title").textContent = titles[s.phase] || "";
  const sub = $("status-sub");
  sub.textContent = s.status_text || "";
  sub.classList.toggle("error", s.status_kind === "error");

  // host card
  $("host-card-name").textContent = s.host || "No host selected";
  $("host-card-dir").textContent = s.remote_dir || "";
  $("host-card-badge").innerHTML = badgeHtml(s.readiness[s.host]);

  renderRecentGrid(s.recent);
  renderHostsList(s);
  renderSettings(s);

  // sidebar brand dot uses accent via CSS; nothing else
  if ($("panel").classList.contains("show")) refreshOpenPanel();
}

function badgeHtml(r) {
  if (!r) return "";
  if (r.state === "ready") return `<span class="badge ready">✓ Ready</span>`;
  if (r.state === "checking") return `<span class="badge checking"><span class="mini-spin"></span>Checking</span>`;
  if (r.state === "failed") return `<span class="badge failed" title="${escapeAttr(r.message)}">Failed</span>`;
  return "";
}

function compactName(name) {
  return (name || "").replace(/^screenshot-/, "");
}

function renderRecentGrid(recent) {
  const grid = $("recent-grid");
  if (!recent || recent.length === 0) {
    grid.innerHTML = `<div class="empty-hint" style="grid-column:1/-1">No uploads yet. Copy an image to upload it.</div>`;
    return;
  }
  grid.innerHTML = "";
  recent.slice(0, 6).forEach((u) => {
    const tile = document.createElement("div");
    tile.className = "recent-tile";
    tile.innerHTML = `<div class="host">${escapeHtml(u.host)}</div><div class="file">${escapeHtml(compactName(u.local_name))}</div>`;
    tile.addEventListener("click", () => copyAndFlash(tile, u.remote_path));
    grid.appendChild(tile);
  });
}

function renderHostsList(s) {
  const list = $("hosts-list");
  if (!s.hosts || s.hosts.length === 0) {
    list.innerHTML = `<div class="empty-hint">No hosts found in ~/.ssh/config. Add one above.</div>`;
    return;
  }
  list.innerHTML = "";
  s.hosts.forEach((h) => {
    const row = document.createElement("div");
    row.className = "host-row";
    const selected = h === s.host;
    row.innerHTML = `
      <div class="icon"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="4" width="18" height="6" rx="1.5"/><rect x="3" y="14" width="18" height="6" rx="1.5"/></svg></div>
      <div class="meta"><div class="name">${escapeHtml(h)}</div><div class="dir">${escapeHtml(badgeText(s.readiness[h]))}</div></div>
      ${selected ? '<span class="check"><svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M5 12l5 5L20 6"/></svg></span>' : ""}
      <button class="icon-btn edit-btn"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><path d="M12 20h9"/><path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4z"/></svg></button>`;
    row.addEventListener("click", (e) => {
      if (e.target.closest(".edit-btn")) return;
      invoke("select_host", { host: h });
    });
    el(".edit-btn", row).addEventListener("click", (e) => {
      e.stopPropagation();
      openEditHost(h);
    });
    list.appendChild(row);
  });
}

function badgeText(r) {
  if (!r) return "";
  if (r.state === "ready") return "Ready";
  if (r.state === "checking") return "Checking…";
  if (r.state === "failed") return "Unreachable";
  return "";
}

function renderSettings(s) {
  setIfNotFocused("set-remote-dir", s.settings.remote_dir);
  setIfNotFocused("set-filename", s.settings.filename_pattern);
  setIfNotFocused("set-screenshot-folder", s.settings.screenshot_folder);
  $("sw-screenshots").classList.toggle("on", s.settings.monitor_screenshots);
  $("sw-autocopy").classList.toggle("on", s.settings.auto_copy_path);
  $("sw-login").classList.toggle("on", s.settings.launch_at_login);

  const sel = $("set-test-host");
  const cur = sel.value;
  sel.innerHTML = s.hosts.map((h) => `<option value="${escapeAttr(h)}"${h === s.host ? " selected" : ""}>${escapeHtml(h)}</option>`).join("");
  if (![...sel.options].some((o) => o.value === s.host)) sel.value = cur;

  const ts = $("test-status");
  ts.textContent = s.test_result || "";
  ts.className = "status-line " + (s.test_result === "Connection OK." ? "ok" : s.test_result ? "err" : "");
}

function setIfNotFocused(id, value) {
  const node = $(id);
  if (document.activeElement !== node) node.value = value ?? "";
}

// --- panels ------------------------------------------------------------------
let panelKind = null;

function openPanel() {
  $("scrim").classList.add("show");
  $("panel").classList.add("show");
}
function closePanel() {
  $("scrim").classList.remove("show");
  $("panel").classList.remove("show");
  panelKind = null;
}
function refreshOpenPanel() {
  if (panelKind === "hostPicker") renderHostPicker();
  else if (panelKind === "recent") renderRecentPanel();
}

function openHostPicker() {
  panelKind = "hostPicker";
  renderHostPicker();
  openPanel();
}

function renderHostPicker() {
  const s = state;
  const body = $("panel-body");
  let inner = `<h2>Choose host</h2><div class="panel-sub">From ~/.ssh/config</div>`;
  if (!s.hosts.length) {
    inner += `<div class="empty-hint">No hosts found.</div>`;
  } else if (s.hosts.length >= 7) {
    inner += `<div>${s.hosts.map((h) => hostRowPicker(h, s)).join("")}</div>`;
  } else {
    inner += `<div class="host-grid">${s.hosts.map((h) => hostTile(h, s)).join("")}</div>`;
  }
  inner += `<div class="btn-row" style="margin-top:16px"><button class="btn" id="picker-reload">Reload</button><button class="btn primary" id="picker-add">+ Add</button></div>`;
  body.innerHTML = inner;
  body.querySelectorAll("[data-host]").forEach((node) =>
    node.addEventListener("click", () => {
      invoke("select_host", { host: node.dataset.host });
      closePanel();
    })
  );
  $("picker-reload").addEventListener("click", () => invoke("reload_hosts"));
  $("picker-add").addEventListener("click", () => {
    closePanel();
    gotoPage("hosts");
    openAddHost();
  });
}

function hostTile(h, s) {
  const sel = h === s.host ? " selected" : "";
  return `<div class="host-tile${sel}" data-host="${escapeAttr(h)}"><div class="icon"><svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2"><rect x="3" y="4" width="18" height="6" rx="1.5"/><rect x="3" y="14" width="18" height="6" rx="1.5"/></svg></div><div class="name">${escapeHtml(h)}</div></div>`;
}
function hostRowPicker(h, s) {
  const check = h === s.host ? '<span class="check"><svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.5"><path d="M5 12l5 5L20 6"/></svg></span>' : "";
  return `<div class="host-row" data-host="${escapeAttr(h)}"><div class="meta"><div class="name">${escapeHtml(h)}</div><div class="dir">${escapeHtml(badgeText(s.readiness[h]))}</div></div>${check}</div>`;
}

function openAddHost() {
  openHostForm(null);
}
async function openEditHost(host) {
  let details;
  try {
    details = await invoke("connection_details", { host });
  } catch {
    details = { alias: host, host_name: "", user: "", port: "", remote_dir: "" };
  }
  openHostForm(details);
}

function openHostForm(details) {
  panelKind = "form";
  const editing = !!details;
  const d = details || { alias: "", host_name: "", user: "", port: "", remote_dir: "" };
  $("panel-body").innerHTML = `
    <h2>${editing ? "Edit host" : "Add host"}</h2>
    <div class="panel-sub">Writes a block in ~/.ssh/config.</div>
    <div class="field"><label>Name (alias)</label><input class="input" id="f-alias" spellcheck="false" value="${escapeAttr(d.alias)}" /></div>
    <div class="field"><label>Server host or IP</label><input class="input" id="f-hostname" spellcheck="false" value="${escapeAttr(d.host_name)}" /></div>
    <div class="field"><label>User (optional)</label><input class="input" id="f-user" spellcheck="false" value="${escapeAttr(d.user)}" /></div>
    <div class="field"><label>Port (optional)</label><input class="input" id="f-port" spellcheck="false" value="${escapeAttr(d.port)}" /></div>
    <div class="field"><label>Remote folder</label><input class="input" id="f-dir" spellcheck="false" placeholder="/tmp/paste2ssh" value="${escapeAttr(d.remote_dir)}" /></div>
    <div class="btn-row">
      <button class="btn primary" id="f-save">${editing ? "Save" : "Add"}</button>
      ${editing ? '<button class="btn danger" id="f-delete">Delete</button>' : ""}
    </div>
    <div class="status-line err" id="f-status"></div>`;
  openPanel();

  $("f-save").addEventListener("click", async () => {
    const msg = await invoke("save_ssh_connection", {
      originalHost: editing ? d.alias : null,
      alias: $("f-alias").value,
      hostName: $("f-hostname").value,
      user: $("f-user").value,
      port: $("f-port").value,
      remoteDir: $("f-dir").value,
    });
    if (msg.startsWith("Added") || msg.startsWith("Saved")) {
      closePanel();
      showToast(msg);
    } else {
      $("f-status").textContent = msg;
    }
  });
  if (editing) {
    $("f-delete").addEventListener("click", async () => {
      const msg = await invoke("delete_ssh_connection", { host: d.alias });
      closePanel();
      showToast(msg);
    });
  }
}

function openRecentPanel() {
  panelKind = "recent";
  renderRecentPanel();
  openPanel();
}
function renderRecentPanel() {
  const body = $("panel-body");
  const recent = state.recent || [];
  let inner = `<h2>Recent uploads</h2><div class="panel-sub">Click to copy the remote path.</div>`;
  if (!recent.length) inner += `<div class="empty-hint">No uploads yet.</div>`;
  else
    inner += recent
      .map(
        (u, i) =>
          `<div class="recent-list-item" data-i="${i}"><div class="file">${escapeHtml(u.local_name)}</div><div class="sub"><span class="path">${escapeHtml(u.remote_path)}</span><span>${timeAgo(u.date_ms)}</span></div></div>`
      )
      .join("");
  body.innerHTML = inner;
  body.querySelectorAll(".recent-list-item").forEach((node) => {
    const u = recent[Number(node.dataset.i)];
    node.addEventListener("click", () => copyAndFlash(node, u.remote_path));
  });
}

// --- helpers -----------------------------------------------------------------
function copyAndFlash(node, path) {
  invoke("copy_path", { path });
  node.classList.add("copied");
  setTimeout(() => node.classList.remove("copied"), 1400);
}

let toastTimer = null;
function showToast(text) {
  const t = $("toast");
  t.textContent = text;
  t.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => t.classList.remove("show"), 3000);
}

function timeAgo(ms) {
  const diff = Date.now() - ms;
  const s = Math.floor(diff / 1000);
  if (s < 60) return "now";
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ago`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h}h ago`;
  return `${Math.floor(h / 24)}d ago`;
}

function escapeHtml(str) {
  return String(str ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" }[c]));
}
function escapeAttr(str) {
  return escapeHtml(str);
}

// --- drag & drop -------------------------------------------------------------
function setupDrop() {
  const overlay = $("drop-overlay");
  listen("tauri://drag-over", () => overlay.classList.add("show"));
  listen("tauri://drag-enter", () => overlay.classList.add("show"));
  listen("tauri://drag-leave", () => overlay.classList.remove("show"));
  listen("tauri://drag-drop", (e) => {
    overlay.classList.remove("show");
    const paths = (e.payload && e.payload.paths) || [];
    if (paths.length) invoke("upload_files", { paths });
  });
}

window.addEventListener("DOMContentLoaded", init);

const token = document.querySelector('meta[name="app-token"]').content;
const $ = (selector) => document.querySelector(selector);
const state = { root: "", rootEdited: false, materials: [], events: [], selected: null, selectedMaterials: new Set(), filter: "all", query: "", selectedAnchors: new Map(), loadedImages: new Set(), scanning: false, folderPath: null };

async function api(path, options = {}) {
  const headers = { "X-App-Token": token, ...(options.headers || {}) };
  if (options.body) headers["Content-Type"] = "application/json";
  const response = await fetch(path, { ...options, headers });
  const type = response.headers.get("content-type") || "";
  const result = type.includes("application/json") ? await response.json() : await response.text();
  if (!response.ok) throw new Error(result?.error || `HTTP ${response.status}`);
  return result;
}
function logLocal(level, message, source = "UI") {
  state.events.push({ timestamp_ms: String(Date.now()), source, level, message });
  if (state.events.length > 500) state.events.shift();
  renderConsole();
}
function esc(value) { return String(value ?? "").replace(/[&<>"']/g, (character) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[character]); }
function currentList() {
  const query = state.query.trim().toLocaleLowerCase();
  let list = state.materials.filter((material) => {
    const matchesQuery = !query || [material.name, material.directory, ...material.textures.map((texture) => `${texture.kind} ${texture.filename}`)].join(" ").toLocaleLowerCase().includes(query);
    const matchesFilter = state.filter === "all" || (state.filter === "missing" && material.textures.length < 5) || (state.filter === "multiple" && material.anchor_candidates.length > 1);
    return matchesQuery && matchesFilter;
  });
  if ($("#sortSelect").value === "name-desc") list = [...list].reverse();
  return list;
}
function anchorFor(material) { return state.selectedAnchors.get(material.index) || material.anchor; }
function imageUrl(path) { return `/api/image?token=${encodeURIComponent(token)}&path=${encodeURIComponent(path)}`; }
function render() {
  const list = currentList();
  $("#summary").textContent = state.scanning ? "Scanning folders and filenames…" : `${state.materials.length.toLocaleString()} materials · ${state.root || "No folder selected"}`;
  $("#filesCount").textContent = Number(window.filesExamined || 0).toLocaleString();
  $("#skippedCount").textContent = Number(window.foldersSkipped || 0).toLocaleString();
  $("#previewCount").textContent = state.loadedImages.size.toLocaleString();
  $("#pageSummary").textContent = `${list.length.toLocaleString()} results`;
  $("#selectedCount").textContent = `${state.selectedMaterials.size.toLocaleString()} selected`;
  $("#scanBtn").disabled = state.scanning;
  $("#scanBtn").innerHTML = state.scanning ? "◌ Scanning…" : "⌕ Scan materials";
  $("#statusText").textContent = state.scanning ? "Scan in progress" : "Ready";
  $(".status-dot").classList.toggle("busy", state.scanning);
  if (!state.materials.length && !state.scanning) {
    $("#grid").innerHTML = `<div class="empty-state"><div class="empty-icon">◇</div><h2>Your materials, at a glance</h2><p>Choose a folder to index filenames and associate PBR maps. Image files are requested only as their cards come into view.</p><button class="button primary" id="emptyBrowse">Choose folder</button></div>`;
    $("#emptyBrowse").onclick = browse;
  } else if (!list.length) {
    $("#grid").innerHTML = `<div class="empty-state"><div class="empty-icon">⌕</div><h2>No results</h2><p>Try another filter or search term.</p></div>`;
  } else {
    $("#grid").innerHTML = list.map((material) => cardHtml(material)).join("");
    $("#grid").querySelectorAll(".material-card").forEach((card) => {
      card.addEventListener("click", () => selectMaterial(Number(card.dataset.index)));
      const check = card.querySelector(".card-check");
      check.addEventListener("click", (event) => { event.stopPropagation(); selectMaterial(Number(card.dataset.index)); });
    });
    $("#grid").querySelectorAll(".preview-image").forEach((image) => {
      image.addEventListener("load", () => { state.loadedImages.add(image.dataset.path); $("#previewCount").textContent = state.loadedImages.size.toLocaleString(); }, { once: true });
      image.addEventListener("error", () => { const wrap = image.closest(".preview-wrap"); if (wrap) wrap.innerHTML = `<div class="preview-placeholder" title="This image format is not supported by the browser">▧</div>`; }, { once: true });
    });
  }
  renderInspector();
}
function cardHtml(material) {
  const selected = state.selectedMaterials.has(material.index);
  const anchor = anchorFor(material);
  const image = imageUrl(anchor.path);
  const tags = material.textures.slice(0, 4).map((texture) => `<span class="map-tag ${texture.path === anchor.path ? "anchor" : ""}">${esc(texture.kind)}</span>`).join("");
  const additional = material.textures.length > 4 ? `<span class="map-tag">+${material.textures.length - 4}</span>` : "";
  return `<article class="material-card ${selected ? "selected" : ""}" data-index="${material.index}"><button type="button" class="card-check" aria-label="${selected ? "Deselect" : "Select"} ${esc(material.name)}" aria-pressed="${selected}">${selected ? "✓" : ""}</button><div class="preview-wrap"><img class="preview-image" src="${image}" data-path="${esc(anchor.path)}" alt="${esc(material.name)}" loading="lazy" decoding="async" referrerpolicy="no-referrer"></div><div class="material-name" title="${esc(material.name)}">${esc(material.name)}</div><div class="material-folder" title="${esc(material.directory)}">${esc(material.directory)}</div><div class="map-tags">${tags}${additional}</div><div class="card-foot"><span>${material.textures.length} maps</span><span class="${material.anchor_candidates.length > 1 ? "card-warning" : ""}">${material.anchor_candidates.length > 1 ? `⚠ ${material.anchor_candidates.length} albedos` : esc(material.anchor.kind)}</span></div></article>`;
}
function renderInspector() {
  const selected = state.materials.filter((item) => state.selectedMaterials.has(item.index));
  const focused = state.materials.find((item) => item.index === state.selected);
  const material = (focused && state.selectedMaterials.has(focused.index) ? focused : selected[0]) || focused;
  const selectedMapCount = selected.reduce((sum, item) => sum + item.textures.length, 0);
  $("#downloadSelectedBtn").disabled = selected.length === 0;
  $("#downloadSelectedBtn").textContent = selected.length ? `Download selection · ${selectedMapCount} maps` : "Download selection";
  $("#copyUrlBtn").disabled = selected.length !== 1;
  $("#openFolderBtn").disabled = selected.length !== 1;
  if (selected.length > 1) {
    $("#selectionTitle").textContent = `${selected.length} materials selected`;
    $("#selectionPath").textContent = `${selectedMapCount} maps · A ZIP will preserve the folder structure.`;
  } else if (selected.length === 1) {
    $("#selectionTitle").textContent = selected[0].name;
    $("#selectionPath").textContent = selected[0].directory;
  } else {
    $("#selectionTitle").textContent = "No material selected";
    $("#selectionPath").textContent = "Select one or more cards to download their maps.";
  }
  if (!material) {
    $("#mapCount").textContent = "0";
    $("#mapsList").innerHTML = `<div class="muted-note">This material's maps will appear here.</div>`;
    return;
  }
  $("#mapCount").textContent = selected.length ? selectedMapCount : material.textures.length;
  const candidates = new Set(material.anchor_candidates.map((texture) => texture.path));
  $("#mapsList").innerHTML = material.textures.map((texture) => `<div class="map-row"><span class="map-icon">▧</span><div class="map-info"><div class="map-kind">${esc(texture.kind)}</div><div class="map-file" title="${esc(texture.path)}">${esc(texture.filename)}</div></div>${candidates.has(texture.path) ? `<button class="map-open" title="Use as image" data-preview-path="${esc(texture.path)}">◉</button>` : ""}</div>`).join("") || `<div class="muted-note">No associated maps found.</div>`;
  $("#mapsList").querySelectorAll("[data-preview-path]").forEach((button) => button.addEventListener("click", async (event) => {
    event.stopPropagation(); const path = button.dataset.previewPath;
    state.selectedAnchors.set(material.index, material.anchor_candidates.find((item) => item.path === path)); render(); logLocal("info", `Primary image selected: ${path}`, "IMAGE");
  }));
}
function selectMaterial(index) {
  state.selected = index;
  if (state.selectedMaterials.has(index)) state.selectedMaterials.delete(index);
  else state.selectedMaterials.add(index);
  render();
}
function selectVisible() {
  const list = currentList();
  const allSelected = list.length && list.every((material) => state.selectedMaterials.has(material.index));
  for (const material of list) allSelected ? state.selectedMaterials.delete(material.index) : state.selectedMaterials.add(material.index);
  render();
}
function renderConsole() {
  const container = $("#console");
  $("#eventCount").textContent = `${state.events.length} events`;
  container.innerHTML = state.events.length ? state.events.map((event) => {
    const date = new Date(Number(event.timestamp_ms));
    const time = Number.isNaN(date.getTime()) ? "--:--:--" : date.toLocaleTimeString("en", { hour12: false });
    return `<div class="event-row ${event.level === "error" ? "error" : ""}"><span class="event-time">${time}</span><span class="event-source">${esc(event.source)}</span><span class="event-level">${esc(event.level).toUpperCase()}</span><span class="event-message" title="${esc(event.message)}">${esc(event.message)}</span></div>`;
  }).join("") : `<div class="console-empty">Material Library is ready. Progress and errors will appear here.</div>`;
  container.scrollTop = container.scrollHeight;
}
async function refreshState() {
  try {
    const data = await api("/api/state");
    if (!state.rootEdited) { state.root = data.root || ""; $("#rootPath").value = state.root; }
    state.materials = data.materials || []; state.events = data.events || [];
    state.scanning = !!data.scanning; window.filesExamined = data.files_examined || 0; window.foldersSkipped = data.folders_skipped || 0;
    render(); renderConsole();
    if (state.scanning && !state.pollTimer) state.pollTimer = setInterval(refreshState, 1000);
    else if (!state.scanning && state.pollTimer) { clearInterval(state.pollTimer); state.pollTimer = null; }
  } catch (error) { $("#statusText").textContent = `Connection error: ${error.message}`; }
}
async function browse() {
  $("#folderModal").hidden = false;
  $("#folderModal").classList.add("visible");
  await loadFolders(state.root || null);
}
async function loadFolders(path) {
  $("#folderList").innerHTML = `<div class="muted-note">Loading subfolders…</div>`;
  $("#folderUse").disabled = true;
  try {
    const result = await api(`/api/folders${path ? `?path=${encodeURIComponent(path)}` : ""}`);
    state.folderPath = result.path;
    $("#folderLocation").textContent = result.path || "Available drives";
    $("#folderUp").disabled = !result.parent;
    $("#folderCount").textContent = `${result.folders.length} folders`;
    $("#folderUse").disabled = !result.path;
    $("#folderList").innerHTML = result.folders.length ? result.folders.map((folder) => `<button class="folder-entry" data-folder-path="${esc(folder.path)}"><span class="folder-entry-icon">▰</span><span class="folder-entry-name">${esc(folder.name)}</span><span class="folder-entry-open">Open ›</span></button>`).join("") : `<div class="folder-empty"><span>◌</span><strong>No visible subfolders</strong><small>You can use this folder or go up one level.</small></div>`;
    $("#folderList").querySelectorAll("[data-folder-path]").forEach((button) => button.onclick = () => loadFolders(button.dataset.folderPath));
    $("#folderUp").onclick = () => result.parent && loadFolders(result.parent);
  } catch (error) {
    $("#folderLocation").textContent = path || "Unidades disponibles";
    $("#folderUp").disabled = !path;
    $("#folderCount").textContent = "";
    $("#folderList").innerHTML = `<div class="folder-empty"><span>!</span><strong>Could not list this folder</strong><small>${esc(error.message)}</small></div>`;
    logLocal("error", `Could not read folder listing: ${error.message}`, "ERROR");
  }
}
function closeFolderBrowser() {
  $("#folderModal").hidden = true;
  $("#folderModal").classList.remove("visible");
}
async function useCurrentFolder() {
  if (!state.folderPath) return;
  try {
    const result = await api("/api/set-root", { method: "POST", body: JSON.stringify({ path: state.folderPath }) });
    state.root = result.path; state.rootEdited = false; $("#rootPath").value = result.path;
    closeFolderBrowser(); logLocal("info", `Selected folder: ${result.path}`, "FILES");
  } catch (error) { logLocal("error", `Could not select folder: ${error.message}`, "ERROR"); }
}
async function scan() {
  const root = $("#rootPath").value.trim(); if (!root) { logLocal("error", "Choose a root folder before scanning.", "ERROR"); return; }
  try { state.root = root; state.rootEdited = false; state.scanning = true; state.materials = []; state.selected = null; state.selectedMaterials.clear(); state.selectedAnchors.clear(); state.loadedImages.clear(); render(); if (!state.pollTimer) state.pollTimer = setInterval(refreshState, 1000); await api("/api/scan", { method: "POST", body: JSON.stringify({ root }) }); }
  catch (error) { state.scanning = false; logLocal("error", `Could not start scan: ${error.message}`, "ERROR"); render(); }
}
function downloadSelected() {
  const indices = [...state.selectedMaterials].sort((left, right) => left - right);
  if (!indices.length) return;
  const link = document.createElement("a");
  link.href = `/download-selected?token=${encodeURIComponent(token)}&materials=${indices.join(",")}`;
  link.download = "MaterialLibrary-textures.zip";
  document.body.append(link); link.click(); link.remove();
  logLocal("info", `Preparing download for ${indices.length} materials and their maps.`, "DOWNLOAD");
}
async function copyUrl() {
  const material = state.materials.find((item) => state.selectedMaterials.has(item.index)); if (!material || state.selectedMaterials.size !== 1) return;
  try { const result = await api(`/api/file-url?path=${encodeURIComponent(anchorFor(material).path)}`); await navigator.clipboard.writeText(result.url); logLocal("info", `URL copied: ${result.url}`, "FILES"); }
  catch (error) { logLocal("error", `Could not copy URL: ${error.message}`, "ERROR"); }
}
async function openFolder() {
  const material = state.materials.find((item) => state.selectedMaterials.has(item.index)); if (!material || state.selectedMaterials.size !== 1) return;
  try { await api("/api/open-folder", { method: "POST", body: JSON.stringify({ path: material.directory }) }); }
  catch (error) { logLocal("error", `Could not open folder: ${error.message}`, "ERROR"); }
}
$("#browseBtn").onclick = browse; $("#scanBtn").onclick = scan;
$("#downloadSelectedBtn").onclick = downloadSelected; $("#copyUrlBtn").onclick = copyUrl; $("#openFolderBtn").onclick = openFolder;
$("#selectVisible").onclick = selectVisible; $("#clearSelection").onclick = () => { state.selectedMaterials.clear(); render(); };
$("#searchInput").addEventListener("input", () => { state.query = $("#searchInput").value; render(); });
$("#sortSelect").onchange = render;
document.querySelectorAll(".chip").forEach((chip) => chip.onclick = () => { document.querySelector(".chip.active")?.classList.remove("active"); chip.classList.add("active"); state.filter = chip.dataset.filter; render(); });
async function clearEvents() { try { await api("/api/events/clear", { method: "POST", body: "{}" }); state.events = []; renderConsole(); } catch (error) { logLocal("error", error.message, "ERROR"); } }
$("#clearBtn").onclick = clearEvents; $("#clearConsole").onclick = clearEvents;
$("#quitBtn").onclick = async () => { try { await api("/api/shutdown", { method: "POST", body: "{}" }); clearInterval(state.pollTimer); $("#statusText").textContent = "Server stopped; you can close this tab."; logLocal("info", "Server stopped.", "SYSTEM"); } catch (error) { logLocal("error", `Could not stop server: ${error.message}`, "ERROR"); } };
$("#folderClose").onclick = closeFolderBrowser; $("#folderCancel").onclick = closeFolderBrowser; $("#folderUse").onclick = useCurrentFolder;
$("#folderModal").addEventListener("click", (event) => { if (event.target === $("#folderModal")) closeFolderBrowser(); });
$("#rootPath").addEventListener("input", (event) => { state.rootEdited = true; state.root = event.target.value; });
$("#rootPath").addEventListener("keydown", (event) => { if (event.key === "Enter") scan(); });
render(); refreshState();

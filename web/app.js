const token = document.querySelector('meta[name="app-token"]').content;
const $ = (selector) => document.querySelector(selector);
const state = { root: "", rootEdited: false, materials: [], events: [], selected: null, selectedMaterials: new Set(), filter: "all", query: "", mapTypeFilter: "", missingMapFilter: "", formatFilter: "", resolutionFilter: "", filterRules: [], selectedAnchors: new Map(), loadedImages: new Set(), favorites: new Set(), favoritesLoaded: false, favoritesSave: Promise.resolve(), facetsSignature: "", scanning: false, folderPath: null };
const missingMapAlternatives = {
  "Normal": ["Normal"],
  "Roughness": ["Roughness", "Glossiness / Smoothness", "Packed ORM", "Packed RMA", "Packed MRA"],
  "AO": ["AO", "Packed ORM", "Packed RMA", "Packed MRA"],
  "Metallic": ["Metallic", "Packed ORM", "Packed RMA", "Packed MRA"],
  "Height": ["Height", "Bump", "Displacement"],
  "Opacity / Alpha": ["Opacity / Alpha"],
  "Specular": ["Specular"]
};

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
function favoriteKey(material) { return `${material.directory.replaceAll("\\", "/").toLocaleLowerCase()}\u001f${material.name.toLocaleLowerCase()}`; }
function fileExtension(texture) {
  const extension = texture.filename?.split(".").pop()?.toLocaleLowerCase() || "";
  return extension === "jpg" ? "jpeg" : extension === "tif" ? "tiff" : extension;
}
function resolutionValue(material) {
  const size = anchorFor(material).dimensions;
  return size ? Math.max(size.width, size.height) : null;
}
function resolutionLabel(material) {
  const size = anchorFor(material).dimensions;
  const maximum = resolutionValue(material);
  const text = maximum === null ? "Unknown" : maximum <= 512 ? "≤512" : maximum <= 1024 ? "1K" : maximum <= 2048 ? "2K" : maximum <= 4096 ? "4K" : maximum <= 8192 ? "8K" : "16K+";
  return { text, title: size ? `${size.width} × ${size.height}` : "Resolution unknown" };
}
function matchesResolution(material, filter) {
  if (!filter) return true;
  const value = resolutionValue(material);
  if (filter === "unknown") return value === null;
  if (value === null) return false;
  const bounds = { "512": [0, 512], "1k": [513, 1024], "2k": [1025, 2048], "4k": [2049, 4096], "8k": [4097, 8192], "16k": [8193, Infinity] }[filter];
  return !!bounds && value >= bounds[0] && value <= bounds[1];
}
function wildcardMatch(value, pattern) {
  const escaped = pattern.replace(/[.+^${}()|[\]\\]/g, "\\$&").replaceAll("*", ".*").replaceAll("?", ".");
  return new RegExp(`^${escaped}$`, "i").test(value);
}
function textMatches(value, query, operator) {
  const text = value.toLocaleLowerCase();
  const pattern = query.toLocaleLowerCase();
  const matched = pattern.includes("*") || pattern.includes("?")
    ? wildcardMatch(text, pattern)
    : operator === "starts" ? text.startsWith(pattern)
      : operator === "ends" ? text.endsWith(pattern)
        : operator === "equals" ? text === pattern
          : pattern.split(/\s+/).filter(Boolean).every((part) => text.includes(part));
  return matched;
}
function ruleMatches(material, rule) {
  const query = rule.value.trim();
  if (!query) return true;
  const negative = rule.operator.startsWith("not_");
  const operator = negative ? rule.operator.slice(4) : rule.operator;
  const candidates = rule.field === "folder" ? [material.directory]
    : rule.field === "file" ? material.textures.map((texture) => texture.filename)
      : rule.field === "map" ? material.textures.map((texture) => texture.kind)
        : [material.name];
  const matched = candidates.some((value) => textMatches(value, query, operator));
  return negative ? !matched : matched;
}
function replaceFacetOptions(select, label, values, selected) {
  select.replaceChildren(new Option(label, ""));
  for (const [value, text] of values) select.add(new Option(text, value));
  select.value = values.some(([value]) => value === selected) ? selected : "";
}
function updateFacetOptions() {
  const mapTypes = [...new Set(state.materials.flatMap((material) => material.textures.map((texture) => texture.kind)))].sort((a, b) => a.localeCompare(b));
  const formats = [...new Set(state.materials.flatMap((material) => material.textures.map(fileExtension).filter(Boolean)))].sort();
  const signature = JSON.stringify([mapTypes, formats]);
  if (signature === state.facetsSignature) return;
  state.facetsSignature = signature;
  replaceFacetOptions($("#mapTypeFilter"), "Any map type", mapTypes.map((value) => [value, value]), state.mapTypeFilter);
  replaceFacetOptions($("#formatFilter"), "Any file format", formats.map((value) => [value, value === "jpeg" ? "JPEG (.jpg, .jpeg)" : value === "tiff" ? "TIFF (.tif, .tiff)" : `.${value.toUpperCase()}`]), state.formatFilter);
  if (!mapTypes.includes(state.mapTypeFilter)) state.mapTypeFilter = "";
  if (!formats.includes(state.formatFilter)) state.formatFilter = "";
}
function renderFilterRules() {
  const container = $("#filterRules");
  container.replaceChildren();
  if (!state.filterRules.length) {
    const empty = document.createElement("div");
    empty.className = "rules-empty";
    empty.textContent = "No custom filters. Add a rule to narrow results.";
    container.append(empty);
    return;
  }
  const fields = [["name", "Material name"], ["folder", "Folder"], ["file", "Map filename"], ["map", "Map type"]];
  const operators = [["contains", "Contains"], ["not_contains", "Does not contain"], ["starts", "Starts with"], ["ends", "Ends with"], ["equals", "Is"], ["not_equals", "Is not"]];
  state.filterRules.forEach((rule, index) => {
    const row = document.createElement("div");
    row.className = "filter-rule";
    const field = document.createElement("select");
    const operator = document.createElement("select");
    const value = document.createElement("input");
    const remove = document.createElement("button");
    for (const [key, label] of fields) field.add(new Option(label, key));
    for (const [key, label] of operators) operator.add(new Option(label, key));
    field.value = rule.field;
    operator.value = rule.operator;
    value.value = rule.value;
    value.placeholder = "Text, * or ?";
    value.setAttribute("aria-label", `Filter rule ${index + 1} value`);
    remove.type = "button";
    remove.className = "remove-rule";
    remove.textContent = "×";
    remove.title = "Remove filter";
    remove.setAttribute("aria-label", `Remove filter ${index + 1}`);
    field.addEventListener("change", () => { rule.field = field.value; render(); });
    operator.addEventListener("change", () => { rule.operator = operator.value; render(); });
    value.addEventListener("input", () => { rule.value = value.value; render(); });
    remove.addEventListener("click", () => { state.filterRules.splice(index, 1); renderFilterRules(); render(); });
    row.append(field, operator, value, remove);
    container.append(row);
  });
}
function addFilterRule() {
  state.filterRules.push({ field: "name", operator: "contains", value: "" });
  renderFilterRules();
  render();
  $("#filterRules input:last-of-type")?.focus();
}
function persistFavorites() {
  const favorites = [...state.favorites];
  state.favoritesSave = state.favoritesSave.catch(() => {}).then(() => api("/api/favorites", {
    method: "POST",
    body: JSON.stringify({ favorites })
  })).catch((error) => logLocal("error", `Could not save favorites: ${error.message}`, "ERROR"));
  return state.favoritesSave;
}
function toggleFavorite(index) {
  const material = state.materials.find((item) => item.index === index);
  if (!material) return;
  const key = favoriteKey(material);
  if (state.favorites.has(key)) state.favorites.delete(key);
  else state.favorites.add(key);
  render();
  persistFavorites();
}
function currentList() {
  const query = state.query.trim().toLocaleLowerCase();
  let list = state.materials.filter((material) => {
    const matchesQuery = !query || [material.name, material.directory, ...material.textures.map((texture) => `${texture.kind} ${texture.filename}`)].join(" ").toLocaleLowerCase().includes(query);
    const matchesQuickFilter = state.filter === "all" || (state.filter === "multiple" && material.anchor_candidates.length > 1) || (state.filter === "favorites" && state.favorites.has(favoriteKey(material)));
    const matchesMapType = !state.mapTypeFilter || material.textures.some((texture) => texture.kind === state.mapTypeFilter);
    const missingAlternatives = missingMapAlternatives[state.missingMapFilter];
    const matchesMissing = !missingAlternatives || !missingAlternatives.some((kind) => material.textures.some((texture) => texture.kind === kind));
    const matchesFormat = !state.formatFilter || material.textures.some((texture) => fileExtension(texture) === state.formatFilter);
    const matchesRules = state.filterRules.every((rule) => ruleMatches(material, rule));
    return matchesQuery && matchesQuickFilter && matchesMapType && matchesMissing && matchesFormat && matchesResolution(material, state.resolutionFilter) && matchesRules;
  });
  const sort = $("#sortSelect").value;
  if (sort === "name-desc") list = [...list].reverse();
  else if (sort === "resolution-desc") list.sort((left, right) => (resolutionValue(right) ?? -1) - (resolutionValue(left) ?? -1));
  else if (sort === "maps-desc") list.sort((left, right) => right.textures.length - left.textures.length);
  return list;
}
function anchorFor(material) { return state.selectedAnchors.get(material.index) || material.anchor; }
function imageUrl(path) { return `/api/image?token=${encodeURIComponent(token)}&path=${encodeURIComponent(path)}`; }
function render() {
  updateFacetOptions();
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
      const favorite = card.querySelector(".favorite-toggle");
      favorite.addEventListener("click", (event) => { event.stopPropagation(); toggleFavorite(Number(card.dataset.index)); });
      const openPreview = card.querySelector(".preview-open");
      openPreview.addEventListener("click", (event) => {
        event.stopPropagation();
        const material = state.materials.find((item) => item.index === Number(card.dataset.index));
        if (!material) return;
        const texture = anchorFor(material);
        window.open(imageUrl(texture.path), "_blank", "noopener,noreferrer");
        logLocal("info", `Opened original image: ${texture.path}`, "IMAGE");
      });
    });
    $("#grid").querySelectorAll(".preview-image").forEach((image) => {
      image.addEventListener("load", () => {
        state.loadedImages.add(image.dataset.path);
        $("#previewCount").textContent = state.loadedImages.size.toLocaleString();
        const material = state.materials.find((item) => item.anchor_candidates.some((candidate) => candidate.path === image.dataset.path));
        const matchingMaps = material ? [material.anchor, ...material.anchor_candidates, ...material.textures].filter((texture) => texture.path === image.dataset.path) : [];
        const anchor = matchingMaps[0];
        if (anchor && (!anchor.dimensions || anchor.dimensions.width !== image.naturalWidth || anchor.dimensions.height !== image.naturalHeight)) {
          for (const texture of matchingMaps) texture.dimensions = { width: image.naturalWidth, height: image.naturalHeight };
          if (state.resolutionFilter || $("#sortSelect").value === "resolution-desc") {
            render();
          } else {
            const label = resolutionLabel(material);
            const resolution = image.closest(".material-card")?.querySelector(".card-foot span:last-child");
            if (resolution) { resolution.textContent = label.text; resolution.title = label.title; }
          }
        }
      }, { once: true });
      image.addEventListener("error", () => { const wrap = image.closest(".preview-wrap"); if (wrap) wrap.innerHTML = `<div class="preview-placeholder" title="This image format is not supported by the browser">▧</div>`; }, { once: true });
    });
  }
  renderInspector();
}
function cardHtml(material) {
  const selected = state.selectedMaterials.has(material.index);
  const favorite = state.favorites.has(favoriteKey(material));
  const anchor = anchorFor(material);
  const image = imageUrl(anchor.path);
  const tags = material.textures.slice(0, 4).map((texture) => `<span class="map-tag ${texture.path === anchor.path ? "anchor" : ""}">${esc(texture.kind)}</span>`).join("");
  const additional = material.textures.length > 4 ? `<span class="map-tag">+${material.textures.length - 4}</span>` : "";
  const resolution = resolutionLabel(material);
  const anchorStatus = material.anchor_candidates.length > 1 ? `⚠ ${material.anchor_candidates.length} albedos` : esc(anchor.kind);
  return `<article class="material-card ${selected ? "selected" : ""}" data-index="${material.index}"><div class="card-actions"><button type="button" class="favorite-toggle ${favorite ? "active" : ""}" aria-label="${favorite ? "Remove from" : "Add to"} favorites: ${esc(material.name)}" aria-pressed="${favorite}" title="${favorite ? "Remove from" : "Add to"} favorites">★</button><button type="button" class="card-check" aria-label="${selected ? "Deselect" : "Select"} ${esc(material.name)}" aria-pressed="${selected}">${selected ? "✓" : ""}</button></div><div class="preview-wrap"><img class="preview-image" src="${image}" data-path="${esc(anchor.path)}" alt="${esc(material.name)}" loading="lazy" decoding="async" referrerpolicy="no-referrer"><button type="button" class="preview-open" title="Open original diffuse/albedo image" aria-label="Open original image for ${esc(material.name)}"><svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="10.8" cy="10.8" r="6.3"></circle><path d="m15.5 15.5 5 5"></path></svg></button></div><div class="material-name" title="${esc(material.name)}">${esc(material.name)}</div><div class="material-folder" title="${esc(material.directory)}">${esc(material.directory)}</div><div class="map-tags">${tags}${additional}</div><div class="card-foot"><span>${material.textures.length} maps · ${anchorStatus}</span><span title="${esc(resolution.title)}">${resolution.text}</span></div></article>`;
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
    if (!state.favoritesLoaded) { state.favorites = new Set(data.favorites || []); state.favoritesLoaded = true; }
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
    $("#folderLocation").textContent = path || "Available drives";
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
$("#mapTypeFilter").onchange = () => { state.mapTypeFilter = $("#mapTypeFilter").value; render(); };
$("#missingMapFilter").onchange = () => { state.missingMapFilter = $("#missingMapFilter").value; render(); };
$("#formatFilter").onchange = () => { state.formatFilter = $("#formatFilter").value; render(); };
$("#resolutionFilter").onchange = () => { state.resolutionFilter = $("#resolutionFilter").value; render(); };
$("#addFilterRule").onclick = addFilterRule;
$("#clearFilterRules").onclick = () => { state.filterRules = []; renderFilterRules(); render(); };
$("#clearAllFilters").onclick = () => {
  state.filter = "all"; state.query = ""; state.mapTypeFilter = ""; state.missingMapFilter = ""; state.formatFilter = ""; state.resolutionFilter = ""; state.filterRules = [];
  $("#searchInput").value = ""; $("#mapTypeFilter").value = ""; $("#missingMapFilter").value = ""; $("#formatFilter").value = ""; $("#resolutionFilter").value = "";
  document.querySelector(".chip.active")?.classList.remove("active"); document.querySelector('.chip[data-filter="all"]').classList.add("active");
  renderFilterRules(); render();
};
async function clearEvents() { try { await api("/api/events/clear", { method: "POST", body: "{}" }); state.events = []; renderConsole(); } catch (error) { logLocal("error", error.message, "ERROR"); } }
$("#clearBtn").onclick = clearEvents; $("#clearConsole").onclick = clearEvents;
$("#quitBtn").onclick = async () => {
  const button = $("#quitBtn");
  button.disabled = true;
  button.textContent = "Quitting…";
  try {
    await state.favoritesSave;
    await api("/api/shutdown", { method: "POST", body: "{}" });
    clearInterval(state.pollTimer);
    document.title = "Material Library — Closed";
    for (const element of document.body.children) element.hidden = element.id !== "quitScreen";
    document.body.classList.add("quit-page");
    $("#quitScreen").hidden = false;
    $("#quitTitle").focus();
  } catch (error) {
    button.disabled = false;
    button.textContent = "Quit";
    logLocal("error", `Could not stop server: ${error.message}`, "ERROR");
  }
};
$("#folderClose").onclick = closeFolderBrowser; $("#folderCancel").onclick = closeFolderBrowser; $("#folderUse").onclick = useCurrentFolder;
$("#folderModal").addEventListener("click", (event) => { if (event.target === $("#folderModal")) closeFolderBrowser(); });
$("#rootPath").addEventListener("input", (event) => { state.rootEdited = true; state.root = event.target.value; });
$("#rootPath").addEventListener("keydown", (event) => { if (event.key === "Enter") scan(); });
render(); refreshState();

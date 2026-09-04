// ARGUS Website Configuration
const ARGUS_DOWNLOAD_URL = "https://github.com/AngKool-Dev/argus-releases/releases/download/v0.1.11/era-launcher.exe";

const RELEASES_API = "https://api.github.com/repos/AngKool-Dev/argus-releases/releases?per_page=100";
const LATEST_RELEASE_API = "https://api.github.com/repos/AngKool-Dev/argus-releases/releases/latest";

async function loadLatestVersion() {
  const nodes = document.querySelectorAll("[data-version]");
  if (!nodes.length) return;
  try {
    const res = await fetch(LATEST_RELEASE_API, {
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!res.ok) throw new Error(String(res.status));
    const data = await res.json();
    const tag = (data.tag_name || "").replace(/^v/, "");
    const size = (data.assets || []).find((a) => a.name === "era-launcher.exe");
    const sizeMB = size ? (size.size / 1024 / 1024).toFixed(1) : "—";
    const text = `v${tag} (${sizeMB} MB)`;
    nodes.forEach((n) => {
      n.textContent = text;
    });
  } catch (_err) {
    // leave existing version text if fetch fails
  }
}

async function loadDownloadCounts() {
  const nodes = document.querySelectorAll("[data-dl-count]");
  if (!nodes.length) return;
  try {
    const res = await fetch(RELEASES_API, {
      headers: { Accept: "application/vnd.github+json" },
    });
    if (!res.ok) throw new Error(String(res.status));
    const releases = await res.json();
    let total = 0;
    for (const rel of releases) {
      for (const asset of rel.assets || []) {
        total += asset.download_count || 0;
      }
    }
    const formatted = new Intl.NumberFormat("en-US").format(total);
    nodes.forEach((n) => {
      n.textContent = formatted;
    });
  } catch (_err) {
    document.querySelectorAll("[data-dl-stat]").forEach((el) => el.remove());
  }
}

loadLatestVersion();
loadDownloadCounts();

document.querySelectorAll("[data-download]").forEach((btn) => {
  btn.addEventListener("click", (e) => {
    e.preventDefault();
    window.location.href = ARGUS_DOWNLOAD_URL;
  });
});

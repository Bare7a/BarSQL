// A saved theme wins. Otherwise follow the system setting until the visitor picks one.
function setUpTheme() {
  const root = document.documentElement;
  const KEY = "barsql-theme";

  const chosen = () => {
    try {
      const theme = localStorage.getItem(KEY);
      return theme === "light" || theme === "dark" ? theme : null;
    } catch {
      return null;
    }
  };

  document.getElementById("theme-toggle").addEventListener("click", () => {
    const next = root.dataset.theme === "dark" ? "light" : "dark";
    root.dataset.theme = next;
    try {
      localStorage.setItem(KEY, next);
    } catch {}
  });

  matchMedia("(prefers-color-scheme: light)").addEventListener("change", (event) => {
    if (!chosen()) root.dataset.theme = event.matches ? "light" : "dark";
  });
}

function detectOS() {
  const ua = navigator.userAgent;
  const platform = navigator.userAgentData?.platform || navigator.platform || "";
  const mobile = /Android|iPhone|iPad|iPod/i.test(ua) || (/Mac/i.test(platform) && navigator.maxTouchPoints > 1);
  if (mobile) return null;
  if (/Mac/i.test(platform) || /Macintosh/.test(ua)) return "macos";
  if (/Win/i.test(platform) || /Windows/.test(ua)) return "windows";
  if (/Linux|X11/i.test(`${platform} ${ua}`)) return "linux";
  return null;
}

// Point the hero button at the visitor's own build, highlight its card and pick the modifier key.
// Phones and tablets keep the generic button, which leads to every download.
function pointDownloadAtVisitor() {
  const os = detectOS();
  if (!os) return;

  const names = { windows: "Windows", macos: "macOS", linux: "Linux" };
  const card = document.getElementById(`dl-${os}`);
  const main = card.querySelector(".dl-main");
  const hero = document.getElementById("hero-download");
  const icon = hero.querySelector("svg");
  hero.href = main.href;
  icon.setAttribute("class", "logo");
  icon.querySelector("use").setAttribute("href", `logos.svg#l-${os === "macos" ? "apple" : os}`);
  document.getElementById("hero-download-label").textContent = `Download for ${names[os]}`;
  document.getElementById("hero-download-meta").textContent = main.dataset.meta;
  card.classList.add("dl-detected");

  if (os === "macos") {
    for (const key of document.querySelectorAll("kbd[data-mod]")) {
      key.textContent = "⌘";
      key.title = "Command";
    }
  }
}

// Lightbox with every screenshot on the page, full size and in order. Arrow keys, swipes and the buttons move between them.
function setUpLightbox() {
  const box = document.getElementById("lightbox");
  const img = box.querySelector("img");
  const caption = box.querySelector(".lightbox-caption");
  const count = box.querySelector(".lightbox-count");
  const closeButton = box.querySelector(".lightbox-close");
  const prevButton = box.querySelector(".lightbox-prev");
  const nextButton = box.querySelector(".lightbox-next");
  const focusable = [closeButton, prevButton, nextButton];
  const shots = [...document.querySelectorAll(".shot[data-shot]")];

  const seen = new Set();
  const items = shots
    .filter((shot) => !seen.has(shot.dataset.shot) && seen.add(shot.dataset.shot))
    .map((shot) => ({ id: shot.dataset.shot, caption: shot.dataset.caption, alt: shot.querySelector("img").alt }))
    .sort((a, b) => a.id - b.id);

  let index = 0;
  let opener = null;
  let touchX = null;

  const source = (item) => `screenshots/${item.id}.webp`;

  const show = (at) => {
    index = (at + items.length) % items.length;
    const item = items[index];
    img.src = source(item);
    img.alt = item.alt;
    caption.textContent = item.caption;
    count.textContent = `${index + 1} / ${items.length}`;
    new Image().src = source(items[(index + 1) % items.length]);
  };

  const open = (id, trigger) => {
    opener = trigger;
    show(Math.max(0, items.findIndex((item) => item.id === id)));
    box.hidden = false;
    document.body.style.overflow = "hidden";
    closeButton.focus();
  };

  const close = () => {
    box.hidden = true;
    document.body.style.overflow = "";
    opener?.focus();
  };

  for (const shot of shots) {
    shot.addEventListener("click", () => open(shot.dataset.shot, shot));
  }

  closeButton.addEventListener("click", close);
  prevButton.addEventListener("click", (event) => {
    event.stopPropagation();
    show(index - 1);
  });
  nextButton.addEventListener("click", (event) => {
    event.stopPropagation();
    show(index + 1);
  });
  img.addEventListener("click", (event) => event.stopPropagation());
  box.addEventListener("click", close);

  document.addEventListener("keydown", (event) => {
    if (box.hidden) return;
    if (event.key === "Escape") close();
    else if (event.key === "ArrowRight") show(index + 1);
    else if (event.key === "ArrowLeft") show(index - 1);
    else if (event.key === "Tab") {
      const at = focusable.indexOf(document.activeElement);
      focusable[(at + (event.shiftKey ? -1 : 1) + focusable.length) % focusable.length].focus();
      event.preventDefault();
    }
  });

  box.addEventListener("touchstart", (event) => {
    touchX = event.touches[0].clientX;
  }, { passive: true });
  box.addEventListener("touchend", (event) => {
    if (touchX === null) return;
    const dx = event.changedTouches[0].clientX - touchX;
    touchX = null;
    if (Math.abs(dx) > 50) show(index + (dx < 0 ? 1 : -1));
  });
}

setUpTheme();
pointDownloadAtVisitor();
setUpLightbox();

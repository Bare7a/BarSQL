// A saved theme wins. Otherwise follow the system setting until the visitor picks one.
(function () {
  var root = document.documentElement;
  var KEY = "barsql-theme";

  function chosen() {
    try {
      var theme = localStorage.getItem(KEY);
      return theme === "light" || theme === "dark" ? theme : null;
    } catch (e) {
      return null;
    }
  }

  document.getElementById("theme-toggle").addEventListener("click", function () {
    var next = root.dataset.theme === "dark" ? "light" : "dark";
    root.dataset.theme = next;
    try {
      localStorage.setItem(KEY, next);
    } catch (e) {}
  });

  if (window.matchMedia) {
    window.matchMedia("(prefers-color-scheme: light)").addEventListener("change", function (event) {
      if (!chosen()) root.dataset.theme = event.matches ? "light" : "dark";
    });
  }
})();

// Label the download button, highlight the download card and pick the modifier key for the visitor's OS.
// Phones and tablets keep the generic button.
(function () {
  var ua = navigator.userAgent;
  var platform = (navigator.userAgentData && navigator.userAgentData.platform) || navigator.platform || "";
  var mobile = /Android|iPhone|iPad|iPod/i.test(ua) || (/Mac/i.test(platform) && navigator.maxTouchPoints > 1);
  var os = null;
  if (!mobile) {
    if (/Mac/i.test(platform) || /Macintosh/.test(ua)) os = "macos";
    else if (/Win/i.test(platform) || /Windows/.test(ua)) os = "windows";
    else if (/Linux|X11/i.test(platform + " " + ua)) os = "linux";
  }
  if (!os) return;

  var names = { windows: "Windows", macos: "macOS", linux: "Linux" };
  document.getElementById("hero-download-label").textContent = "Download for " + names[os];
  var card = document.getElementById("dl-" + os);
  if (card) card.classList.add("dl-detected");

  if (os === "macos") {
    document.querySelectorAll("kbd[data-mod]").forEach(function (key) {
      key.textContent = "⌘";
      key.title = "Command";
    });
  }
})();

// Lightbox with the full-size WebP screenshots. Arrow keys, swipes and the buttons move between them.
(function () {
  var box = document.getElementById("lightbox");
  var img = box.querySelector("img");
  var caption = box.querySelector(".lightbox-caption");
  var count = box.querySelector(".lightbox-count");
  var buttons = {
    close: box.querySelector(".lightbox-close"),
    prev: box.querySelector(".lightbox-prev"),
    next: box.querySelector(".lightbox-next"),
  };
  var focusable = [buttons.close, buttons.prev, buttons.next];
  var items = Array.prototype.map.call(document.querySelectorAll(".gallery .shot"), function (shot) {
    return { id: shot.dataset.shot, caption: shot.dataset.caption, alt: shot.querySelector("img").alt };
  });
  var index = 0;
  var opener = null;
  var touchX = null;

  function source(item) {
    return "screenshots/" + item.id + ".webp";
  }

  function show(at) {
    index = (at + items.length) % items.length;
    var item = items[index];
    img.src = source(item);
    img.alt = item.alt;
    caption.textContent = item.caption;
    count.textContent = index + 1 + " / " + items.length;
    new Image().src = source(items[(index + 1) % items.length]);
  }

  function open(id, trigger) {
    var at = 0;
    for (var i = 0; i < items.length; i++) {
      if (items[i].id === id) at = i;
    }
    opener = trigger;
    show(at);
    box.hidden = false;
    document.body.style.overflow = "hidden";
    buttons.close.focus();
  }

  function close() {
    box.hidden = true;
    document.body.style.overflow = "";
    if (opener) opener.focus();
  }

  document.querySelectorAll(".shot").forEach(function (shot) {
    shot.addEventListener("click", function () {
      open(shot.dataset.shot, shot);
    });
  });

  buttons.close.addEventListener("click", close);
  buttons.prev.addEventListener("click", function (event) {
    event.stopPropagation();
    show(index - 1);
  });
  buttons.next.addEventListener("click", function (event) {
    event.stopPropagation();
    show(index + 1);
  });
  img.addEventListener("click", function (event) {
    event.stopPropagation();
  });
  box.addEventListener("click", close);

  document.addEventListener("keydown", function (event) {
    if (box.hidden) return;
    if (event.key === "Escape") close();
    else if (event.key === "ArrowRight") show(index + 1);
    else if (event.key === "ArrowLeft") show(index - 1);
    else if (event.key === "Tab") {
      var at = focusable.indexOf(document.activeElement);
      focusable[(at + (event.shiftKey ? -1 : 1) + focusable.length) % focusable.length].focus();
      event.preventDefault();
    }
  });

  box.addEventListener("touchstart", function (event) {
    touchX = event.touches[0].clientX;
  }, { passive: true });
  box.addEventListener("touchend", function (event) {
    if (touchX === null) return;
    var dx = event.changedTouches[0].clientX - touchX;
    touchX = null;
    if (Math.abs(dx) > 50) show(index + (dx < 0 ? 1 : -1));
  });
})();

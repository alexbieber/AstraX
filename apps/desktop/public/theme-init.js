(function () {
  var theme = "dark";
  try {
    var stored = window.localStorage.getItem("codexx.theme");
    theme = stored === "light" ? "light" : "dark";
  } catch (_error) {
    // Keep the dark default when storage is unavailable.
  }
  document.documentElement.dataset.theme = theme;
  document.documentElement.style.colorScheme = theme;
})();

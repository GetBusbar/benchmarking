/* The evidence pages use the dashboard's stylesheet and theme preference. */
document.getElementById("theme-toggle")?.addEventListener("click", () => {
  const theme = document.documentElement.getAttribute("data-theme") === "light" ? "dark" : "light";
  document.documentElement.setAttribute("data-theme", theme);
  try { localStorage.setItem("theme", theme); } catch {}
});

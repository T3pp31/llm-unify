const menuButton = document.querySelector(".menu-button");
const navigation = document.querySelector("#navigation");
menuButton.hidden = false;
navigation.dataset.open = "false";
function closeMenu() {
  navigation.dataset.open = "false";
  menuButton.setAttribute("aria-expanded", "false");
}
menuButton.addEventListener("click", () => {
  const open = menuButton.getAttribute("aria-expanded") !== "true";
  menuButton.setAttribute("aria-expanded", String(open));
  navigation.dataset.open = String(open);
});
navigation.addEventListener("click", (event) => {
  if (event.target.closest("a")) closeMenu();
});
document.addEventListener("keydown", (event) => {
  if (
    event.key === "Escape" &&
    menuButton.getAttribute("aria-expanded") === "true"
  ) {
    closeMenu();
    menuButton.focus();
  }
});
for (const button of document.querySelectorAll("[data-copy]")) {
  button.addEventListener("click", async () => {
    const code = document.getElementById(button.dataset.copy);
    const status = document.getElementById("copy-status");
    try {
      await navigator.clipboard.writeText(code.textContent);
      button.textContent = "Copied!";
      status.textContent = "Code copied to clipboard.";
    } catch {
      button.textContent = "Select code";
      const range = document.createRange();
      range.selectNodeContents(code);
      const selection = window.getSelection();
      selection.removeAllRanges();
      selection.addRange(range);
      status.textContent =
        "Clipboard unavailable. Code selected; use your keyboard to copy.";
    }
  });
}

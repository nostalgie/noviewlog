/** Tiny DOM builders shared by the chrome bar and the filter panel. */

export function el(tag: string, className: string): HTMLElement {
  const e = document.createElement(tag);
  e.className = className;
  return e;
}

export function button(label: string, title: string, onClick: () => void): HTMLButtonElement {
  const b = document.createElement("button");
  b.textContent = label;
  b.title = title;
  b.onclick = onClick;
  return b;
}

export function toggleButton(
  label: string,
  title: string,
  initial: boolean,
  onChange: (on: boolean) => void,
): HTMLButtonElement {
  const b = button(label, title, () => {
    const on = b.getAttribute("aria-pressed") !== "true";
    b.setAttribute("aria-pressed", String(on));
    onChange(on);
  });
  b.setAttribute("aria-pressed", String(initial));
  return b;
}

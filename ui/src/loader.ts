/** A plain ring spinner (the name is kept from the old hextech animation). */
export function hextechLoader(container: HTMLElement): () => void {
  const ring = document.createElement("span");
  ring.className = "spinner-ring spinner-ring-large";
  container.classList.add("spinner-host");
  container.append(ring);
  return () => ring.remove();
}

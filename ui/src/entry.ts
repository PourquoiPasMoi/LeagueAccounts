import { failStartup } from "./startup";
import { hextechLoader } from "./loader";

const loader = document.querySelector<HTMLElement>("#startup-animation");
if (loader) hextechLoader(loader);

// Keep the HTML loading screen usable even when an app module fails to load.
document.querySelector("#startup-retry")?.addEventListener("click", () => location.reload());
void import("./main").catch(failStartup);

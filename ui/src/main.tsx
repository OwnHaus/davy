import "./styles.css";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { queries } from "./api/client";
import { keepSecret } from "./pages/Welcome";
import { router } from "./router";

// The welcome link carries its secret after `#`. Take it out of the address
// bar before anything renders, so it stays out of history and screenshots.
if (window.location.hash) {
  if (window.location.pathname === "/ui/welcome") keepSecret(window.location.hash.slice(1));
  history.replaceState(history.state, "", window.location.pathname + window.location.search);
}

const root = document.getElementById("root");
if (root) {
  createRoot(root).render(
    <StrictMode>
      <QueryClientProvider client={queries}>
        <RouterProvider router={router} />
      </QueryClientProvider>
    </StrictMode>,
  );
}

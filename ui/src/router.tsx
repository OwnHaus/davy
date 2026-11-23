import { Outlet, createRootRoute, createRoute, createRouter, redirect } from "@tanstack/react-router";
import { Issue } from "./pages/Issue";
import { Issues } from "./pages/Issues";
import { Login } from "./pages/Login";
import { Logs } from "./pages/Logs";
import { NotFound } from "./pages/Shell";
import { Welcome } from "./pages/Welcome";
import { issueSearch, issuesSearch, loginSearch, logsSearch, parseSearch, stringifySearch } from "./search";

const root = createRootRoute({ component: Outlet, notFoundComponent: NotFound });

const routes = [
  createRoute({ getParentRoute: () => root, path: "/login", validateSearch: loginSearch, component: Login }),
  createRoute({ getParentRoute: () => root, path: "/welcome", component: Welcome }),
  createRoute({
    getParentRoute: () => root,
    path: "/",
    beforeLoad: () => {
      throw redirect({ to: "/issues", replace: true });
    },
  }),
  createRoute({ getParentRoute: () => root, path: "/issues", validateSearch: issuesSearch, component: Issues }),
  createRoute({ getParentRoute: () => root, path: "/issues/$id", validateSearch: issueSearch, component: Issue }),
  createRoute({ getParentRoute: () => root, path: "/logs", validateSearch: logsSearch, component: Logs }),
];

export const router = createRouter({
  routeTree: root.addChildren(routes),
  basepath: "/ui",
  parseSearch,
  stringifySearch,
  defaultPreload: false,
  scrollRestoration: false,
});

declare module "@tanstack/react-router" {
  interface Register {
    router: typeof router;
  }
}

import { Outlet, createRootRoute, createRoute, createRouter, redirect } from "@tanstack/react-router";
import { queries } from "./api/client";
import { AccountPage } from "./pages/Account";
import { Alerts } from "./pages/Alerts";
import { Issue } from "./pages/Issue";
import { Issues } from "./pages/Issues";
import { Login } from "./pages/Login";
import { Logs } from "./pages/Logs";
import { Monitor } from "./pages/Monitor";
import { Monitors } from "./pages/Monitors";
import { Projects } from "./pages/Projects";
import { SetupPage } from "./pages/Setup";
import { NotFound, setupOpen, setupQuery } from "./pages/Shell";
import { Welcome } from "./pages/Welcome";
import {
  alertsSearch,
  issueSearch,
  issuesSearch,
  loginSearch,
  logsSearch,
  monitorsSearch,
  parseSearch,
  projectsSearch,
  stringifySearch,
} from "./search";

const root = createRootRoute({ component: Outlet, notFoundComponent: NotFound });

const routes = [
  createRoute({ getParentRoute: () => root, path: "/login", validateSearch: loginSearch, component: Login }),
  createRoute({ getParentRoute: () => root, path: "/welcome", component: Welcome }),
  createRoute({
    getParentRoute: () => root,
    path: "/",
    // A new server opens on its setup steps until the first error arrives.
    beforeLoad: async () => {
      const setup = await queries.fetchQuery(setupQuery).catch(() => undefined);
      const fresh = setupOpen(setup) && setup?.steps.some((s) => s.what === "error" && s.at === null);
      throw redirect({ to: fresh ? "/setup" : "/issues", replace: true });
    },
  }),
  createRoute({ getParentRoute: () => root, path: "/issues", validateSearch: issuesSearch, component: Issues }),
  createRoute({ getParentRoute: () => root, path: "/issues/$id", validateSearch: issueSearch, component: Issue }),
  createRoute({ getParentRoute: () => root, path: "/logs", validateSearch: logsSearch, component: Logs }),
  createRoute({ getParentRoute: () => root, path: "/monitors", validateSearch: monitorsSearch, component: Monitors }),
  createRoute({ getParentRoute: () => root, path: "/monitors/$id", component: Monitor }),
  createRoute({ getParentRoute: () => root, path: "/alerts", validateSearch: alertsSearch, component: Alerts }),
  createRoute({ getParentRoute: () => root, path: "/projects", validateSearch: projectsSearch, component: Projects }),
  createRoute({ getParentRoute: () => root, path: "/setup", component: SetupPage }),
  createRoute({ getParentRoute: () => root, path: "/account", component: AccountPage }),
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

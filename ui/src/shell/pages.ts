/**
 * The page table. One lazy import per page: each page is its own chunk, so
 * opening the dashboard never parses the providers drawer or the activity
 * charts. The shell renders these; nothing else needs to know they exist.
 */
import { lazy } from "react";

export const Overview = lazy(() => import("../pages/Overview"));
export const Models = lazy(() => import("../pages/Models"));
export const Providers = lazy(() => import("../pages/Providers"));
export const Accounts = lazy(() => import("../pages/Accounts"));
export const Takeover = lazy(() => import("../pages/Takeover"));
export const Cutover = lazy(() => import("../pages/Cutover"));
export const Routing = lazy(() => import("../pages/Routing"));
export const Activity = lazy(() => import("../pages/Activity"));
export const Settings = lazy(() => import("../pages/Settings"));

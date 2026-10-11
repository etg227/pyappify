import React from "react";
import ReactDOM from "react-dom/client";
import {lazy, Suspense} from "react";
import HubApp from "./hub/HubApp";

const LegacyApp = lazy(() => import('./App'));
const legacy = new URLSearchParams(window.location.search).get('mode') === 'legacy';

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <Suspense fallback={<p role="status">正在打开工具中心…</p>}>
      {legacy ? <LegacyApp /> : <HubApp />}
    </Suspense>
  </React.StrictMode>,
);

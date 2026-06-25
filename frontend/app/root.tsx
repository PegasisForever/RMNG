import {
  Links,
  Meta,
  Outlet,
  Scripts,
  ScrollRestoration,
} from "react-router";

import type { Route } from "./+types/root";
import "./app.css";

export function meta() {
  return [{ title: "kasm control" }];
}

export function Layout({ children }: { children: React.ReactNode }) {
  return (
    <html lang="en">
      <head>
        <meta charSet="utf-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1" />
        <Meta />
        <Links />
      </head>
      <body>
        {children}
        <ScrollRestoration />
        <Scripts />
      </body>
    </html>
  );
}

export default function App() {
  return <Outlet />;
}

export function ErrorBoundary({ error }: Route.ErrorBoundaryProps) {
  let message = "Something went wrong";
  let detail = "";
  if (error instanceof Error) {
    message = error.message;
    detail = error.stack ?? "";
  }
  return (
    <main className="mx-auto max-w-2xl p-8">
      <h1 className="text-xl font-semibold text-red-600">{message}</h1>
      {detail ? (
        <pre className="mt-4 overflow-auto rounded bg-slate-100 p-4 text-xs text-slate-500">
          {detail}
        </pre>
      ) : null}
    </main>
  );
}

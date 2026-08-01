---
name: pure-ui
description: Use when asked to build a React page or screen end to end, or to refactor an existing one for Storybook. Triggers when a component reads data (GraphQL, REST, or a store), navigates, or mutates. Delivers the complete working suite: a container wired to real data and navigation, a pure presentational View, a reducer when needed, fixtures, and Storybook stories. Works with any data transport.
license: Apache 2.0
user-invocable: true
---

# Pure UI

## Overview

A pure UI component is a function of its props. This skill splits any React
screen into a presentational `*View` that renders from props alone, plus a thin
container that injects the impure parts: data reads, navigation, mutations, and
global state. Stories rebuild the props from fixtures.

The data transport is a container concern. A View never knows whether its data
came from GraphQL, REST, or a store, so one story format works for all of them.

This skill always targets **Storybook** and **React**. When you ask it to build a
page, it delivers the complete working suite, not just a Storybook:

1. A `*Page` container wired to real data, navigation, and mutations.
2. A `*View` that renders from props with no network, no router, no store.
3. A reducer, when the View owns real state-machine logic.
4. Fixtures, a whole-page story, and stories for the important pieces.

The container is the shippable page. The View plus stories is the design surface.
Build both. "Build this page" means the wired container too, not a View alone.

## When to use

- Building a new screen or component and you want it in Storybook from the start.
- A component will not mount in Storybook because it calls a data hook, a router,
  or a mutation.
- A story throws on a missing provider, a `useQuery`, `useMutation`, `useNavigate`,
  a `<Link>`, or a store/context read.
- You want a design surface for states that are hard to reach in the running app
  (empty, loading, error, negative totals, long lists).

When NOT to use: the component already renders from props with no side effects.
Then skip the split and go straight to "Writing the story".

## The split: three roles

| Role | Suffix | Owns | In Storybook? |
|---|---|---|---|
| Container | `*Page`, `*Container` | Data reads, routing, mutations, wiring | No |
| Presentational | `*View` | Layout and markup from props, callbacks out | Yes |
| Reducer | `*Reducer`, `use*` | State and transitions, no UI, no network | Not directly |

Add a reducer only when the state logic is complex or has become complex, like a
multi-step form or a draft document. Start with `useState`. Promote to a reducer
when transitions multiply or one edit must recompute several fields.

## The four seams

Every reason a component resists Storybook is one of four impure dependencies.
Each has one injection method. Move the dependency to the container and pass the
result down.

| Impure dependency | Symptom in the leaf | Inject as |
|---|---|---|
| Reading data | `useQuery`, store read, `fetch`, cache read | A prop: a view-model object or a masked fragment |
| Navigating | `useNavigate`, `<Link>`, opening a route-owned modal | A callback prop: `onOpenX(id)`, `onBackToList` |
| Mutating / side-effect API | `useMutation`, direct `fetch` POST | A callback prop `onSave`, or an injected API object |
| Session / global context | theme, locale, currency, auth read | A provider decorator in `preview`, not a per-leaf hook |

Ephemeral local UI state may stay in the leaf: a collapsible's open flag, a hover,
an input draft. Anything a user would call "go somewhere" or "open that overlay"
is a callback prop, never an in-leaf route.

## Workflow A: new UI from scratch

1. Write the `*View` first. Type its props. Render from props only.
2. For each piece of data it needs, add a prop. Do not reach for a hook.
3. For each action, add a callback prop. Name it by destination or intent.
4. Build a fixture in `__fixtures__/` for each prop. Add a `*.stories.tsx`.
5. Write the `*Page` container. Wire it to real data, navigation, and mutations.
   Handle loading and error there. Pass resolved props to the View.
6. Route to the `*Page`, not the `*View`. Confirm the page runs in the real app.

Building the View before the container keeps the prop surface honest. The
container has to satisfy the props the View already declared. The job is done
when the `*Page` renders in the running app and the `*View` renders in Storybook.

## Workflow B: refactor an existing component

1. Read the component. List every hook, `<Link>`, and network call. Each is a seam.
2. Create `*View` with the current JSX. Leave `*Page` (or the old name) as the shell.
3. For each seam, delete the hook from the View and add a prop or callback.
4. Move every removed hook into the container. Wire callbacks to the real
   `navigate`, mutation, or store there.
5. Add fixtures and a story. Run Storybook. The View must render with zero
   providers beyond the global ones.

Refactor one seam at a time and keep the app compiling between steps.

## Building the container

The container owns every impure dependency: routing, the data read, mutations,
loading, and error. It resolves them and hands the View plain props and
callbacks. It never appears in Storybook.

```tsx
// InvoiceDetailPage.tsx: the shippable page. Swap the hooks for your transport.
export function InvoiceDetailPage() {
  const { id } = useParams()
  const navigate = useNavigate()
  const { data, loading, error } = useInvoice(id) // GraphQL, REST, or a store
  const [saveInvoice, { loading: saving }] = useSaveInvoice()

  if (loading) return <PageSpinner />
  if (error || !data) return <PageError onRetry={/* refetch */} />

  return (
    <InvoiceDetailView
      invoice={data}
      saving={saving}
      onSave={(patch) => saveInvoice(id, patch)}
      onBackToList={() => navigate("..")}
      onOpenPayment={(pid) => navigate(`payments/${pid}`)}
    />
  )
}
```

Page-level loading and error live in the container. Content states the user reads
as part of the screen (an empty list, a validation banner, a negative total) are
props the View renders, so they get their own stories.

## Choosing a prop shape by transport

Pick the shape from how the data reaches the container. The View stays identical.

| Transport | Prop shape | Fixture style |
|---|---|---|
| Pre-computed values | Plain view-model object (`percent`, `formattedAmount`) | Export a frozen object or a `make*(overrides)` builder |
| GraphQL | Masked fragment typed as `FragmentType<typeof XFragment>` | `makeFragmentData(data, XFragment)`, read `graphql.md` |
| REST / RPC | Plain DTO, plus an injected API object for mutations | `make*(overrides)` builders plus a stub API |
| Large multi-section form | One editable model plus an `updateField(key, value)` callback | `makeState(overrides)` layered on a default |

**When the transport is GraphQL, read `graphql.md` in this skill directory before
typing the prop.** Fragment colocation has rules that break stories if missed.

For REST, inject the API through a context or a prop, never a direct `fetch` in
the leaf. The app passes the real client. Stories pass a stub whose calls resolve
a fixture and log to the Actions panel.

## What goes in Storybook

Story the whole page, and story its important pieces. Do not stop at leaf
components.

- The page `*View` gets a whole-page story, one per meaningful state: default,
  empty, loading-content, error, and edge values. This is the entire page in
  isolation, built from fixtures.
- Each sub-component with its own states gets its own colocated story: rows,
  cards, editors, chips, empty states, headers, totals strips.
- Every modal, drawer, popover, and sheet is its own story, never rendered open
  inside the parent page story. The parent opens it through a callback, and the
  story wires that callback with `linkTo` to the overlay's own story.
- Story a piece when it has variants worth seeing alone or is reused across
  screens. Skip a trivial wrapper. Never story a container.

Group by title: `Area/Pages/FooView` for the whole page, `Area/Components/FooRow`
for the pieces. Reuse the same fixtures across both levels.

## Writing the story

Colocate `Foo.stories.tsx` next to `Foo.tsx`. Drive it from fixtures. Use `fn()`
for plain callbacks so clicks show in the Actions panel.

```tsx
import type { Meta, StoryObj } from "@storybook/react-vite"
import { fn } from "storybook/test"
import { InvoiceDetailView } from "./InvoiceDetailView"
import { invoice, invoiceOverdue } from "../__fixtures__/invoice"
import { toInvoiceList } from "../__fixtures__/storyLinks"

const base = {
  invoice,
  saving: false,
  onSave: fn(),
  onBackToList: toInvoiceList, // navigation callback, see below
}

const meta = {
  title: "Billing/Pages/InvoiceDetailView",
  component: InvoiceDetailView,
  parameters: { layout: "fullscreen" },
} satisfies Meta<typeof InvoiceDetailView>
export default meta
type Story = StoryObj<typeof meta>

export const Default: Story = { args: { ...base } }
export const Overdue: Story = { args: { ...base, invoice: invoiceOverdue } }
export const Saving: Story = { args: { ...base, saving: true } }
```

Wire navigation callbacks with `linkTo` from `@storybook/addon-links` so a click
jumps to the destination story. Share the links in `__fixtures__/storyLinks.ts`:

```tsx
import { linkTo } from "@storybook/addon-links"
export const toInvoiceList = linkTo("Billing/Pages/InvoiceListView", "Populated")
```

Storybook 10 caveat: `linkTo(title, ExportName)` can silently no-op because the
manager does not camel-case-split the export name into a story id. If a link does
nothing, navigate by explicit story id instead:

```ts
import { navigate } from "@storybook/addon-links"
import { storyNameFromExport, toId } from "storybook/internal/csf"

export function linkToStory(title: string, exportName: string) {
  const name = exportName.includes(" ") ? exportName : storyNameFromExport(exportName)
  navigate({ storyId: toId(title, name) })
}
```

## Interactive stories

Ship one interactive story per screen so the UI responds to state changes live.
Keep the static per-state stories too. They are the review snapshots. The
interactive story is for play. Pick the state holder by what the View takes.

The View takes `dispatch` (it has a reducer): wrap it in `useReducer` with the
real reducer. Every edit runs the production state machine.

```tsx
import { useReducer } from "react"
import { documentReducer, initialDoc } from "./documentReducer"

export const Interactive: Story = {
  render: function Render(args) {
    const [document, dispatch] = useReducer(documentReducer, initialDoc)
    return <BudgetFormView {...args} document={document} dispatch={dispatch} />
  },
  args: { ...base },
}
```

The View takes plain value props and `onChange` callbacks (no reducer): use
`useArgs` and write each change back to args. Controls stay in sync.

```tsx
import { useArgs } from "storybook/preview-api" // Storybook 8+

export const Editable: Story = {
  render: function Render(args) {
    const [{ name }, updateArgs] = useArgs()
    return <InvoiceView {...args} name={name} onNameChange={(v) => updateArgs({ name: v })} />
  },
  args: { ...base },
}
```

Two rules: name the render function so hooks pass the rules-of-hooks lint, and
default to `useState` or `useArgs`. Reach for a reducer only when the state logic
is complex or has become complex.

## Fixtures

- Colocate in a `__fixtures__/` folder next to the stories.
- Prefer a `make*(overrides)` builder over hand-rolling an object per story.
- Layer variants on a base: spread the base, override one field.
- If the type has a parser or schema, run the fixture through it so drift fails
  loudly instead of rendering wrong.
- Keep values realistic. A fixture is also design reference.

## Global providers belong in preview

Put context every story needs in `.storybook/preview` as decorators: router,
theme, locale, currency, an app navigation provider. Add a per-story decorator
only when one component needs extra context. Do not add a provider inside a leaf
to make a story pass. That is a sign the data or navigation belongs in a prop.

## Common mistakes

| Mistake | Fix |
|---|---|
| Leaf calls `useQuery` or `useMutation` | Move it to the container. Pass data as a prop, action as a callback. |
| `useNavigate` or `<Link>` left in a `*View` | Replace with a callback prop. Wire `linkTo` in the story. |
| Mocking the network in a story (MSW, `MockedProvider` with mocks) | Extract a container. Stories use fixtures, not network mocks. |
| Apollo cache-reading `useFragment` in a leaf | Use codegen `unmaskFragment` on a passed fragment. See `graphql.md`. |
| One giant object hand-built per story | A `make*(overrides)` builder plus a shared base. |
| Story of a fetch-cache-render flow | That is integration. It belongs in e2e, not Storybook. |
| Adding a provider inside the leaf to render | The dependency should be a prop. Move it to the container. |

## Quick reference

1. Build the `*View` from props. Turn each of the four seams into a prop or callback.
2. Pick the prop shape from the transport. For GraphQL read `graphql.md`.
3. Story the whole page, its pieces, and each modal or drawer. Add one interactive story.
4. Build the `*Page` container. Wire real data, navigation, mutations, loading, and error.
5. Route to the `*Page`. Confirm the page runs in the app and the View renders in Storybook.

Add a reducer only when the state logic is complex. Otherwise use `useState` or `useArgs`.

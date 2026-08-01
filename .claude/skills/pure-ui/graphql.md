# GraphQL transport for pure UI

Read this when a Storybook-bound View needs GraphQL data. It covers the fragment
colocation pattern with graphql-codegen's client preset. The goal is unchanged: a
`*View` that renders from props, with no Apollo read of its own.

Assumes graphql-codegen with the client preset (`gql()`, `FragmentType`,
`makeFragmentData`, `unmaskFragment` in the generated module). If your repo uses
plain typed documents instead, treat a fragment's TypeScript type as the prop and
build fixtures as plain objects.

## Two prop shapes for GraphQL

Pick one per leaf.

1. View-model. The container reads the query, computes plain values, and passes a
   plain object. Best when the backend already returns a computed blob, or the
   View needs derived values (`percent`, `formattedAmount`). Fixtures are plain
   objects. No codegen helpers needed. Prefer this when it fits.

2. Masked fragment. The leaf declares the exact fields it needs as a fragment and
   takes `FragmentType<typeof XFragment>`. Best when the leaf maps closely to a
   server type and you want field-level colocation. Fixtures use
   `makeFragmentData`.

## The masked fragment pattern

Colocate the fragment with the component. Export it so stories can build data.

```tsx
// InvoiceRow.tsx
import { gql, FragmentType, unmaskFragment } from "@/gql" // your generated module

export const InvoiceRowFragment = gql(`
  fragment InvoiceRow on Invoice {
    id
    number
    total { amount }
    status
  }
`)

export function InvoiceRow(props: {
  invoice: FragmentType<typeof InvoiceRowFragment>
  onOpen: (id: string) => void
}) {
  const invoice = unmaskFragment(InvoiceRowFragment, props.invoice)
  return (
    <button onClick={() => onOpen(invoice.id)}>
      {invoice.number} ({invoice.status})
    </button>
  )
}
```

Four rules make this work in Storybook:

1. Type the prop as `FragmentType<typeof XFragment>`, never the raw type.
2. Unmask with codegen `unmaskFragment`, never Apollo's `useFragment`. Apollo's
   version reads the cache, so every story would have to seed the cache first.
3. Export the fragment document from the component. Stories import it.
4. The leaf reads no query and runs no mutation. The parent composes fragments
   and does the single read.

## Fixtures with makeFragmentData

`makeFragmentData(data, XFragment)` returns a typed value shaped like the masked
fragment. The `data` argument is checked against the fragment's fields, so a fixture
that drifts from the schema fails to compile.

```tsx
// __fixtures__/invoice.ts
import { makeFragmentData } from "@/gql"
import { InvoiceRowFragment } from "../InvoiceRow"

export const invoiceRow = makeFragmentData(
  { id: "inv-1", number: "2026-014", total: { amount: 4200 }, status: "PAID" },
  InvoiceRowFragment,
)
```

Pass it straight into the story args:

```tsx
export const Paid: Story = { args: { invoice: invoiceRow, onOpen: fn() } }
```

## Composing fragments up to the container

The container spreads child fragments into its query and passes each masked slice
down. The container stays out of Storybook.

```tsx
// InvoiceListPage.tsx
const InvoiceListQuery = gql(`
  query InvoiceList {
    invoices { id ...InvoiceRow }
  }
`)

export function InvoiceListPage() {
  const { data } = useQuery(InvoiceListQuery)
  const navigate = useNavigate()
  return (
    <InvoiceListView
      invoices={data?.invoices ?? []}
      onOpen={(id) => navigate(id)}
    />
  )
}
```

## Mutations

A View never calls `useMutation`. Expose an action callback such as `onSave` or
`onArchive`. The container owns the mutation and passes the handler plus a
`saving` flag down. Stories pass `fn()` and toggle `saving` in a separate story.

## The MockedProvider escape hatch

Do not mock the network to make a leaf renderable. If a component needs Apollo
context just to render, that is the signal to extract a container.

Two narrow cases justify a story-level `MockedProvider mocks={[...]}`:

1. A component genuinely reads Apollo context and cannot be split further.
2. A presentational leaf renders a small self-contained child that runs its own
   query, and an empty global `MockedProvider` returns empty without crashing.

In both cases the mock is a story-level decorator, not a per-leaf hook, and you
never drag a whole page or container into Storybook just because a mock makes it
possible.

## Optional guardrails

To keep leaves pure over time, an ESLint import boundary can forbid `*View` files
from importing `@apollo/client`, the router, or the store. A CI `build-storybook`
run catches a story that drifted from a renamed prop or a changed fragment.

## Checklist

1. Choose view-model or masked fragment. Prefer view-model when it fits.
2. For a fragment: type the prop `FragmentType<typeof X>`, unmask with
   `unmaskFragment`, export the fragment.
3. Build fixtures with `makeFragmentData`, one `make*` builder if there are variants.
4. Keep every query and mutation in the container.
5. Reach for `MockedProvider` only in the two narrow cases above.

<script lang="ts">
  import type { TableData, TableCell } from "$lib/ocr/tableData";
  let { table, label }: { table: TableData; label: string } = $props();
  const displayRows = $derived.by(() => {
    const anchors = new Map<string, TableCell>();
    const covered = new Set<string>();
    for (const cell of table.cells) {
      anchors.set(cell.row + ":" + cell.column, cell);
      for (let row = cell.row; row < cell.row + cell.rowSpan; row++) {
        for (let column = cell.column; column < cell.column + cell.columnSpan; column++) covered.add(row + ":" + column);
      }
    }
    return Array.from({ length: table.rows }, (_, row) => {
      const cells: { column: number; cell: TableCell | null }[] = [];
      for (let column = 0; column < table.columns; column++) {
        const key = row + ":" + column;
        const cell = anchors.get(key);
        if (cell) cells.push({ column, cell });
        else if (!covered.has(key)) cells.push({ column, cell: null });
      }
      return cells;
    });
  });
</script>

<section class="table-grid-preview">
  <h4>{label}</h4>
  <p>{table.rows} rows × {table.columns} columns · {table.cells.length} cells</p>
  {#if table.rows}
    <!-- svelte-ignore a11y_no_noninteractive_tabindex (The scrollable preview needs focus for native arrow-key and PageUp/PageDown scrolling.) -->
    <div class="grid-scroll" tabindex="0" role="region" aria-label={label + " scroll area"}>
      <table aria-label={label}><tbody>
        {#each displayRows as row, rowIndex (rowIndex)}<tr>
          {#each row as item (item.cell?.id ?? "gap-" + item.column)}
            {#if item.cell}
              {@const cell = item.cell}
              <svelte:element this={cell.header ? "th" : "td"} rowspan={cell.rowSpan} colspan={cell.columnSpan}
                class:header-cell={cell.header} data-preview-cell-id={cell.id}
                aria-label={"Row " + (cell.row + 1) + ", column " + (cell.column + 1) + (cell.header ? ", header" : "")}
                title={"Row " + (cell.row + 1) + ", column " + (cell.column + 1) + ". Spans " + cell.rowSpan + " rows and " + cell.columnSpan + " columns."}>
                {#if cell.text.length}<pre>{cell.text}</pre>{:else}<span class="empty-cell">Empty</span>{/if}
                {#if cell.rowSpan > 1 || cell.columnSpan > 1}<small>{cell.rowSpan} × {cell.columnSpan} span</small>{/if}
              </svelte:element>
            {:else}<td class="gap-cell" aria-label={"No cell at row " + (rowIndex + 1) + ", column " + (item.column + 1)}><span aria-hidden="true">—</span></td>{/if}
          {/each}
        </tr>{/each}
      </tbody></table>
    </div>
  {:else}<p class="empty-table">Empty table</p>{/if}
</section>

<style>
  .table-grid-preview { min-width: 0; }
  h4 { margin: 0 0 6px; color: #cfdef4; font-size: 13px; }
  p { margin: 0 0 9px; color: #9eb7d3; font-size: 11px; line-height: 1.6; }
  .grid-scroll { max-width: 100%; max-height: 330px; overflow: auto; border: 1px solid #425b79; border-radius: 5px; }
  table { border-collapse: collapse; min-width: 100%; text-align: start; }
  td, th { min-width: 80px; max-width: 240px; padding: 8px; vertical-align: top; border: 1px solid #3e5470; background: #122238; color: #dce9fb; }
  .header-cell { background: #263e5a; font-weight: 600; }
  pre { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font: inherit; font-size: 12px; line-height: 1.7; }
  small { display: block; margin-top: 7px; color: #a2bada; font-size: 9px; font-weight: normal; }
  .empty-cell, .gap-cell, .empty-table { color: #92a7c0; font-size: 11px; font-style: italic; }
  .gap-cell { text-align: center; }
  .empty-table { padding: 14px; border: 1px dashed #4c6380; border-radius: 5px; }
  .grid-scroll:focus-visible { outline: 2px solid #a5d1ff; outline-offset: 2px; }
</style>

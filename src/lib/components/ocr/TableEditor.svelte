<script lang="ts">
  type Cell = {
    id: string; row: number; column: number; rowSpan: number; columnSpan: number;
    text: string; header?: boolean;
  };
  type DisplayCell = { column: number; cell: Cell | null };
  let { rows, columns, cells, disabled = false, onedit, onendedit }: {
    rows: number;
    columns: number;
    cells: Cell[];
    disabled?: boolean;
    onedit: (cellId: string, text: string) => void;
    onendedit: () => void;
  } = $props();

  const totalText = $derived(cells.reduce((sum, cell) => sum + cell.text.length, 0));
  const displayRows = $derived.by(() => {
    const anchors = new Map<string, Cell>();
    const covered = new Set<string>();
    for (const cell of cells) {
      anchors.set(cell.row + ":" + cell.column, cell);
      for (let row = cell.row; row < cell.row + cell.rowSpan; row++) {
        for (let column = cell.column; column < cell.column + cell.columnSpan; column++) {
          covered.add(row + ":" + column);
        }
      }
    }
    return Array.from({ length: rows }, (_, row) => {
      const result: DisplayCell[] = [];
      for (let column = 0; column < columns; column++) {
        const key = row + ":" + column;
        const cell = anchors.get(key);
        if (cell) result.push({ column, cell });
        else if (!covered.has(key)) result.push({ column, cell: null });
      }
      return result;
    });
  });
</script>

<div class="table-editor" data-extraction-table-editor>
  <p class="table-shape">{rows} rows × {columns} columns · {cells.length} cells</p>
  <div class="table-scroll">
    <table aria-label="Extracted table cells">
      <tbody>
        {#each displayRows as row, rowIndex (rowIndex)}
          <tr>
            {#each row as item (item.cell?.id ?? "gap-" + item.column)}
              {#if item.cell}
                {@const cell = item.cell}
                <svelte:element this={cell.header ? "th" : "td"} rowspan={cell.rowSpan} colspan={cell.columnSpan}
                  class:header-cell={cell.header} data-table-cell-id={cell.id}>
                  <textarea value={cell.text} disabled={disabled} data-extraction-editor data-extraction-cell={cell.id}
                    aria-label={"Row " + (cell.row + 1) + ", column " + (cell.column + 1)} maxlength={Math.max(0, 1_000_000 - totalText + cell.text.length)}
                    title={(cell.header ? "Header cell. " : "") + (cell.rowSpan > 1 || cell.columnSpan > 1 ? "Spans " + cell.rowSpan + " rows and " + cell.columnSpan + " columns." : "Table cell")}
                    rows={Math.min(5, Math.max(1, cell.text.split("\n").length))}
                    oninput={(event) => onedit(cell.id, event.currentTarget.value)} onblur={onendedit} dir="auto"></textarea>
                </svelte:element>
              {:else}
                <td class="unrecognized-cell" aria-label={"No extracted cell at row " + (rowIndex + 1) + ", column " + (item.column + 1)}><span aria-hidden="true">—</span></td>
              {/if}
            {/each}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  <p class="table-help">Edit cell text directly; Tab moves through cells. Header cells and merged spans stay in the project. CSV contains a rectangular text grid, with empty positions under merged cells.</p>
</div>

<style>
  .table-shape { margin: 6px 0 10px; color: #b6cce7; font-size: 11px; }
  .table-scroll { max-height: 520px; overflow: auto; border: 1px solid #415775; border-radius: 5px; }
  table { width: 100%; border-collapse: collapse; }
  td, th { min-width: 105px; border: 1px solid #3b506b; padding: 0; vertical-align: top; background: #122238; }
  textarea { display: block; box-sizing: border-box; width: 100%; min-width: 105px; min-height: 42px; max-height: 240px; border: 0; border-radius: 0; padding: 10px 8px; background: transparent; color: #dfebfc; font: inherit; font-size: 12px; line-height: 1.6; resize: vertical; }
  .header-cell { background: #223a56; }
  .header-cell textarea { font-weight: 600; }
  .unrecognized-cell { color: #8595aa; text-align: center; padding: 10px; }
  .table-help { margin: 10px 0 0; color: #9eb5d0; font-size: 11px; line-height: 1.65; }
  textarea:focus-visible { outline: 2px solid #a6d2ff; outline-offset: -2px; }
  textarea:disabled { opacity: .55; }
</style>

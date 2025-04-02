<script lang="ts">
	import '../app.css';

	import { Button } from 'bits-ui';
	import type { Snippet } from 'svelte';

	let {
		children,
		href,
		class: class_name = '',
		...attributes
	}: {
		children: Snippet;
		href?: string;
		class?: string;
	} = $props();
</script>

<Button.Root {href} class="button {class_name}" {...attributes}>{@render children()}</Button.Root>

<style>
	.button {
		@apply border border-black bg-white text-black shadow-sm transition duration-300 ease-in-out;
	}
	.button :hover {
		@apply bg-gray-800 text-white;
	}
	.button :active {
		@apply bg-black text-white shadow-none;
	}
</style>

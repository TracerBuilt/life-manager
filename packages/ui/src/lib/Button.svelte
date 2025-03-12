<script lang="ts">
	import { Button } from 'bits-ui';
	import type { Snippet } from 'svelte';

	const {
		children,
		class: class_name = '',
		primary = false,
		size = 'medium',
		background_color = 'blue-600',
		onclick,
		...attributes
	}: {
		children: Snippet;
		class?: string;
		primary?: boolean;
		size?: 'small' | 'medium' | 'large';
		background_color?: string;
		label: string;
		onclick?: () => void;
	} = $props();
</script>

<Button.Root
	class="rounded-md px-4 py-2 font-medium shadow-sm transition-all
		{primary
		? `bg-${background_color} text-white hover:bg-${background_color}`
		: 'bg-neutral-200 text-neutral-800 hover:bg-neutral-300'}
		{size === 'small' ? 'text-sm' : size === 'large' ? 'px-6 py-3 text-lg' : 'text-base'}
		{background_color}
		{class_name}"
	{onclick}
	{...attributes}
>
	{@render children()}
</Button.Root>

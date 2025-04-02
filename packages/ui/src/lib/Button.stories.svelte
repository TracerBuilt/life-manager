<script lang="ts">
  import { defineMeta } from '@storybook/addon-svelte-csf';

  import Button from './Button.svelte';

  const { Story } = defineMeta({
    component: Button
  })
</script>

<Story name="Default" >
   {#snippet children(args)}
  <Button {...args}>Reactive Component</Button>
  {/snippet}
</Story>
<Story name="With Custom Class" args={{ class: 'font-semibold' }} >
    {#snippet children(args)}
        <Button {...args}>Button</Button>

    {/snippet}
</Story>

<Story name="As Link" args={{ href: 'https://example.com' }} >
    {#snippet children(args)}
        <Button {...args}>Button</Button>

    {/snippet}
</Story>

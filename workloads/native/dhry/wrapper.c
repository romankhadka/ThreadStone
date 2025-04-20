// #include "dhry.h"
// #include <string.h> // for strcpy/strcmp prototypes

// /*  Make the original dhry_1.c ‘main’ inert ------------------------------ */
// #define main dhry_unused_main
// int dhry_unused_main(void); /* forward declaration to keep the compiler happy */

// /*  External forward for the one‑iteration driver inside dhry_1.c -------- */
// void Proc_0(void); /* Proc_0 is the loop body called by main */

// /*  Our public symbol: run `runs` iterations and return the count */
// int dhry(int runs)
// {
//     for (int i = 0; i < runs; ++i)
//     {
//         Proc_0();
//     }
//     return runs;
// }

#include "dhry.h"
#include <string.h> // for strcpy/strcmp prototypes

/*  Make the original dhry_1.c ‘main’ inert ------------------------------ */
#define main dhry_unused_main
int dhry_unused_main(void); /* forward declaration to keep the compiler happy */

/*  External forward for the one‑iteration driver inside dhry_1.c -------- */
void Proc_0(void); /* Proc_0 is the loop body called by main */

/*  Our public symbol: run exactly DHRY_ITERS iterations and return that */
#include <stddef.h> /* for size_t */
size_t dhry(size_t _ignored)
{
    for (int i = 0; i < DHRY_ITERS; ++i)
    {
        Proc_0();
    }
    return DHRY_ITERS;
}

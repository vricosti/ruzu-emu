# Direct Maxwell IR to MSL Backend

Implémenter un backend natif MSL pour le renderer Metal de ruzu, générant directement du MSL depuis l’IR
  Maxwell du `shader_recompiler`, sans passer par SPIR-V ni SPIRV-Cross.

  Objectifs impératifs :

  - Conserver le frontend Maxwell et l’IR communs existants.
  - Ajouter un backend MSL dédié dans `shader_recompiler`, avec une structure parallèle au backend SPIR-V
  lorsque pertinent.
  - Ne pas dupliquer la traduction Maxwell ni les analyses/passes communes.
  - Concevoir les interfaces afin que les métadonnées de shaders, bindings, ressources, attributs et execution modes restent partagées entre SPIR-V et MSL.
  - Brancher le backend MSL uniquement sur le renderer Metal.
  - Conserver le backend SPIR-V pour Vulkan et comme oracle de comparaison pendant la migration.
  - Supprimer la dépendance SPIRV-Cross du chemin Metal uniquement lorsque le backend MSL couvre réellement
  tous les shaders utilisés.
  - Préserver les ABI de ressources attendues par `metal_graphics_pipeline.rs` et `metal_compute_pipeline.rs`, ou les faire évoluer explicitement avec leurs consommateurs.
  - Respecter la sémantique Maxwell pour les types, conversions, précision, `NoContraction`, contrôle de flux, discard/demote, atomiques, images, samplers, CBUF, SSBO, mémoire locale/partagée, attributs et sorties.
  - Ne pas remplacer les fonctionnalités manquantes par des stubs, valeurs par défaut silencieuses ou shaders
  de secours.

  Procéder par étapes vérifiables :

  1. Auditer le chemin actuel complet :
     Maxwell ISA → IR → SPIR-V → SPIRV-Cross → MSL → compilation Metal.
  2. Identifier précisément les interfaces réutilisables et les dépendances SPIR-V actuellement exposées au
  renderer Metal.
  3. Définir les structures propres au backend MSL : contexte d’émission, gestion des valeurs SSA, types,
  déclarations, fonctions, ressources, interfaces d’entrée/sortie et metadata.
  4. Porter d’abord un shader minimal vertex/fragment, puis compute.
  5. Ajouter progressivement toutes les familles d’opcodes et interfaces nécessaires, sans casser Vulkan.
  6. Comparer pour chaque shader le MSL direct avec le MSL produit actuellement par SPIRV-Cross, puis comparer le rendu et les résultats GPU.
  7. Ajouter des tests déterministes pour les types, expressions, contrôle de flux, bindings, images,
  atomiques et entry points.
  8. Ajouter un mode temporaire de validation capable de compiler les deux chemins et de journaliser les
  divergences, sans instrumentation coûteuse par défaut.
  9. Une fois la couverture complète validée, faire du MSL direct le chemin par défaut du renderer Metal et
  retirer SPIRV-Cross de ce chemin.

  Contraintes :

  - Travailler sur une branche dédiée issue de `origin/main`.
  - Examiner le code Eden en lecture seule pour préserver les contrats du frontend et de l’IR, même si Eden ne
  possède pas de backend MSL direct.
  - Préserver l’ownership des méthodes et séparer les fichiers selon les composants correspondants du backend
  SPIR-V.
  - Ne pas introduire une seconde IR ou une architecture spécifique Metal qui obligerait à réécrire les
  optimisations communes.
  - Corriger immédiatement toute petite divergence confirmée avec les contrats existants.
  - Faire des commits cohérents et poussables après chaque tranche fonctionnelle.
  - Recompiler `ruzu` et `ruzu-cmd` après chaque étape importante.
  - Tester au minimum les homebrews existants, STK et MK8D, puis vérifier l’absence de régression Vulkan.
  - Ne pas déclarer le travail terminé tant que les shaders graphics et compute utilisés par les tests passent
  sans fallback SPIR-V.

  Critères de fin :

  - Le renderer Metal ne génère et ne consomme plus de SPIR-V.
  - Aucun appel à SPIRV-Cross n’existe dans son chemin normal.
  - Vulkan continue d’utiliser le backend SPIR-V sans régression.
  - Les pipelines graphics et compute Metal sont générés directement depuis l’IR.
  - Les tests du `shader_recompiler` et de `video_core` passent.
  - Les titres de validation démarrent et rendent correctement.
  - Les performances de compilation et d’exécution sont mesurées avant/après.
  - Les limitations restantes sont explicitement identifiées, sans fallback ou dette masquée.

## Completion audit (2026-08-23)

- The normal Metal runtime compiles graphics and compute shaders directly from the shared Maxwell IR to MSL. It does not emit or consume SPIR-V and does not call SPIRV-Cross.
- `spirv-cross2` is no longer a normal `video_core` dependency. It is available only to tests and to builds that explicitly enable `video_core/metal-spirv-validation`; `RUZU_VALIDATE_DIRECT_MSL` warns when used without that feature.
- Vulkan remains on the SPIR-V backend. Metal and Vulkan Pinball smoke tests both exceeded 512 queued frames without shader failures or panics.
- The MK8D persistent Metal cache rebuilt 1,041 direct-MSL pipelines. A cold 550-pipeline rebuild measured 0.254 s with direct MSL versus 3.796 s through SPIRV-Cross (about 14.95x faster). Runtime presentation throughput was effectively unchanged (about 59.55 versus 60.30 queued frames/s).
- `cargo test -p shader_recompiler --release`: 534 passed, 0 failed. `cargo test -p video_core --release -- --test-threads=1`: 1,609 passed, 0 failed, 1 ignored.
- Remaining explicit limitations are unmerged `VertexA`, tessellation-control/evaluation, geometry shaders, FP64, sparse residency, and unsupported host features detected by `first_unsupported_program_feature`. These paths return errors; there is no SPIR-V fallback. None was encountered by the validated homebrew, STK, or MK8D shader sets.
